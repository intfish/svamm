//! shell completion generation and installation.
//!
//! `svamm completions <shell>` prints a completion script generated from the
//! clap command tree to stdout, so users can redirect it into a directory
//! their shell loads completions from. `svamm setup` installs scripts for both
//! the `svamm` binary and its `svm` abbreviation and provisions the `svm`
//! command symlink.
//!
//! the generated scripts are augmented with a shell-specific block that
//! completes live sandbox names (via `<bin> list -q`) for the commands that
//! take an existing sandbox name. the block degrades gracefully: when the
//! backend cannot be reached it falls back to the static completions, and it
//! never completes names for commands that do not take one.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::CommandFactory;
use clap_complete::{aot, Generator, Shell};

use crate::cli;
use crate::config;
use crate::paths;
use crate::ui;

/// the binaries completions are generated for: `svamm` and its `svm` alias.
pub const BIN_NAMES: [&str; 2] = ["svamm", "svm"];

/// commands whose single positional argument is the name of an existing
/// sandbox. these get live name completion from `list -q`.
const NAME_COMMANDS: [&str; 6] = ["net", "start", "stop", "restart", "rm", "run"];

/// render the completion script for `shell` targeting `bin_name`.
fn render(shell: Shell, bin_name: &str) -> anyhow::Result<Vec<u8>> {
	let mut cmd = cli::Cli::command();
	let mut buf = Vec::new();
	aot::generate(shell, &mut cmd, bin_name, &mut buf);
	append_dynamic_block(shell, bin_name, &mut buf);
	Ok(buf)
}

/// append live sandbox-name completion support to the static script.
///
/// bash and fish get a self-contained appended block. zsh gets a rewrite of
/// the generated `_arguments` spec so the name positional routes to a
/// generated function instead of `_default`. the dynamic completer must be
/// defined before the generated trailing invocation. zsh autoload executes
/// the whole file as the function body on the first completion attempt, so a
/// helper defined after the invocation is not available when `_arguments`
/// first resolves the name positional.
fn append_dynamic_block(shell: Shell, bin_name: &str, out: &mut Vec<u8>) {
	let block = match shell {
		Shell::Bash => dynamic_bash(bin_name),
		Shell::Zsh => dynamic_zsh(bin_name),
		Shell::Fish => dynamic_fish(bin_name),
		_ => return,
	};
	if shell == Shell::Zsh {
		let script = String::from_utf8_lossy(out).into_owned();
		let spec = format!(":name -- Sandbox name:_{bin_name}_names");
		let script = script.replace(":name -- Sandbox name:_default", &spec);
		let guard = format!(
			"if [ \"$funcstack[1]\" = \"_{bin_name}\" ]; then\n    _{bin_name} \"$@\"\nelse\n    compdef _{bin_name} {bin_name}\nfi"
		);
		if let Some(index) = script.find(&guard) {
			let mut with_block = String::with_capacity(script.len() + block.len() + 1);
			with_block.push_str(&script[..index]);
			with_block.push('\n');
			with_block.push_str(&block);
			with_block.push_str(&script[index..]);
			out.clear();
			out.extend_from_slice(with_block.as_bytes());
			return;
		}
		// fallback: the generated script has no expected invocation guard.
		// append the block after it instead.
		out.clear();
		out.extend_from_slice(script.as_bytes());
	}
	out.extend_from_slice(b"\n");
	out.extend_from_slice(block.as_bytes());
}

/// the bash block: a wrapper around the generated `_<name>` function.
///
/// the wrapper fills in sandbox names at the name position of the
/// name-taking commands and delegates everything else to the static tree.
fn dynamic_bash(bin_name: &str) -> String {
	format!(
		r#"# live sandbox-name completion for the commands that take a name.
_{bin_name}_with_names() {{
    local _w _names
    case "${{COMP_WORDS[1]}}" in
        net|start|stop|restart|rm|run) ;;
        *) _{bin_name} "$@"; return 0 ;;
    esac
    if [[ "$2" == -* ]]; then
        _{bin_name} "$@"
        return 0
    fi
    for _w in "${{COMP_WORDS[@]:2:COMP_CWORD-2}}"; do
        [[ "$_w" == -* ]] || {{ _{bin_name} "$@"; return 0; }}
    done
    _names="$({bin_name} list -q 2>/dev/null)"
    if [[ -n "$_names" ]]; then
        COMPREPLY=( $(compgen -W "$_names" -- "$2") )
        return 0
    fi
    _{bin_name} "$@"
}}
if [[ "${{BASH_VERSINFO[0]}}" -eq 4 && "${{BASH_VERSINFO[1]}}" -ge 4 || "${{BASH_VERSINFO[0]}}" -gt 4 ]]; then
    complete -F _{bin_name}_with_names -o nosort -o bashdefault -o default {bin_name}
else
    complete -F _{bin_name}_with_names -o bashdefault -o default {bin_name}
fi
"#
	)
}

/// the zsh block: the completion function the rewritten name spec routes to.
fn dynamic_zsh(bin_name: &str) -> String {
	format!(
		r#"# live sandbox-name completion for the commands that take a name.
_{bin_name}_names() {{
    local -a names
    names=("${{(@f)$({bin_name} list -q 2>/dev/null)}}")
    (( ${{#names}} )) && compadd -- "$names[@]"
}}
"#
	)
}

/// the fish block: a helper guarding the name position plus a completion
/// source that runs `list -q` at tab time.
fn dynamic_fish(bin_name: &str) -> String {
	format!(
		r#"function __{bin_name}_needs_name
    set -l cmd (commandline -opc)
    test (count $cmd) -ge 2; or return 1
    set -e cmd[1]
    contains -- $cmd[1] {name_commands}; or return 1
    set -e cmd[1]
    for tok in $cmd
        string match -q -- '-'* "$tok"; or return 1
    end
    return 0
end
complete -c {bin_name} -n "__{bin_name}_needs_name" -a "({bin_name} list -q 2>/dev/null)"
"#,
		name_commands = NAME_COMMANDS.join(" ")
	)
}

/// `svamm completions` - print the completion script for the requested shell.
pub fn run(args: &cli::CompletionCli) -> anyhow::Result<()> {
	let buf = render(args.shell, &args.name)?;
	let mut stdout = std::io::stdout().lock();
	stdout
		.write_all(&buf)
		.map_err(|err| anyhow::anyhow!("failed to write completion script: {err}"))
}

/// `svamm setup` completions step.
///
/// detects the login shell and installs completion scripts for every binary
/// name into the standard per-user location. never edits shell rc files.
pub fn install() -> anyhow::Result<()> {
	let Some(shell) = detect_shell() else {
		ui::warn_note(
			"could not detect the login shell; install completions manually: \
			 `svamm completions <shell>`",
		);
		return Ok(());
	};
	ui::step(format!("installing {shell} completions"));
	match shell {
		Shell::Bash => install_bash()?,
		Shell::Zsh => install_zsh()?,
		Shell::Fish => install_fish()?,
		other => install_other(other)?,
	}
	ui::ok("completions installed");
	Ok(())
}

/// `svamm setup` step making sure both `svamm` and `svm` resolve to this
/// binary from a user bin directory.
pub fn ensure_command_links() -> anyhow::Result<()> {
	let bin_dir = paths::bin_dir();
	let target = std::env::current_exe().context("resolve current executable path")?;
	for name in BIN_NAMES {
		ensure_command_link(&bin_dir, name, &target)?;
	}
	if !config::dry_run() && !bin_dir_on_path(&bin_dir) {
		ui::warn_note(format!(
			"{} is not on PATH; add it, e.g. export PATH=\"$PATH:{}\"",
			bin_dir.display(),
			bin_dir.display()
		));
	}
	Ok(())
}

/// write the completion script for `shell`/`bin_name` to `path`.
fn write_script(shell: Shell, bin_name: &str, path: &Path) -> anyhow::Result<()> {
	let buf = render(shell, bin_name)?;
	if config::dry_run() {
		println!(
			"svamm setup: would write {} completions to {}",
			bin_name,
			path.display()
		);
		return Ok(());
	}
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent)
			.with_context(|| format!("failed to create {}", parent.display()))?;
	}
	std::fs::write(path, buf).with_context(|| format!("failed to write {}", path.display()))?;
	ui::note(format!(
		"wrote {} completions to {}",
		bin_name,
		path.display()
	));
	Ok(())
}

/// the login shell, from `$SHELL`.
fn detect_shell() -> Option<Shell> {
	let shell = std::env::var("SHELL").ok()?;
	Shell::from_shell_path(&shell)
}

/// the directory bash-completion (v2) auto-loads per-user completions from.
fn bash_completions_dir() -> PathBuf {
	paths::home_dir().join(".local/share/bash-completion/completions")
}

/// the directory to place zsh completion functions on `fpath`.
fn zsh_site_functions_dir() -> PathBuf {
	match std::env::var_os("ZDOTDIR") {
		Some(zdotdir) if !zdotdir.is_empty() => PathBuf::from(zdotdir).join("site-functions"),
		_ => paths::home_dir().join(".local/share/zsh/site-functions"),
	}
}

/// the directory fish auto-loads per-user completions from.
fn fish_completions_dir() -> PathBuf {
	let base = std::env::var_os("XDG_CONFIG_HOME")
		.filter(|value| !value.is_empty())
		.map(PathBuf::from)
		.unwrap_or_else(|| paths::home_dir().join(".config"));
	base.join("fish/completions")
}

fn install_bash() -> anyhow::Result<()> {
	let dir = bash_completions_dir();
	for name in BIN_NAMES {
		write_script(Shell::Bash, name, &dir.join(format!("{name}.bash")))?;
	}
	ui::note(format!(
		"bash-completion auto-loads files from {}; without it, source the script in ~/.bashrc:",
		dir.display()
	));
	for name in BIN_NAMES {
		println!("  source <({name} completions bash)");
	}
	Ok(())
}

fn install_zsh() -> anyhow::Result<()> {
	let dir = zsh_site_functions_dir();
	for name in BIN_NAMES {
		write_script(Shell::Zsh, name, &dir.join(format!("_{name}")))?;
	}
	ui::note(format!(
		"add {} to fpath before compinit (e.g. in ~/.zshrc):",
		dir.display()
	));
	println!("          fpath+=({})", dir.display());
	println!("          autoload -Uz compinit && compinit");
	Ok(())
}

fn install_fish() -> anyhow::Result<()> {
	let dir = fish_completions_dir();
	for name in BIN_NAMES {
		write_script(Shell::Fish, name, &dir.join(format!("{name}.fish")))?;
	}
	ui::note(format!(
		"fish auto-loads completions from {}; no rc changes needed",
		dir.display()
	));
	Ok(())
}

fn install_other(shell: Shell) -> anyhow::Result<()> {
	ui::note("no standard per-user install location for this shell; generate the script manually:");
	for name in BIN_NAMES {
		println!(
			"  {name} completions {shell} --name {name} > <completions-dir>/{}",
			shell.file_name(name)
		);
	}
	Ok(())
}

/// create `bin_dir/{name}` as a symlink to `target` when `name` is not
/// already resolvable from PATH.
fn ensure_command_link(bin_dir: &Path, name: &str, target: &Path) -> anyhow::Result<()> {
	if command_on_path(name) {
		if !config::dry_run() {
			ui::note(format!("{name} already available on PATH"));
		}
		return Ok(());
	}
	let link = bin_dir.join(name);
	if config::dry_run() {
		println!(
			"svamm setup: would link {} -> {}",
			link.display(),
			target.display()
		);
		return Ok(());
	}
	if let Ok(meta) = std::fs::symlink_metadata(&link) {
		if meta.file_type().is_symlink() {
			if std::fs::read_link(&link)
				.map(|path| path == target)
				.unwrap_or(false)
			{
				return Ok(());
			}
			std::fs::remove_file(&link).context("remove stale command link")?;
		} else {
			ui::note(format!(
				"{} exists and is not a symlink; leaving it",
				link.display()
			));
			return Ok(());
		}
	}
	std::fs::create_dir_all(bin_dir)
		.with_context(|| format!("failed to create {}", bin_dir.display()))?;
	std::os::unix::fs::symlink(target, &link)
		.with_context(|| format!("failed to link {} -> {}", link.display(), target.display()))?;
	ui::note(format!("linked {} -> {}", link.display(), target.display()));
	Ok(())
}

/// true when an executable named `name` resolves from the current PATH.
fn command_on_path(name: &str) -> bool {
	use std::os::unix::fs::PermissionsExt;
	std::env::var_os("PATH")
		.map(|path| {
			std::env::split_paths(&path).any(|dir| {
				let candidate = dir.join(name);
				candidate.is_file()
					&& candidate
						.metadata()
						.map(|meta| meta.permissions().mode() & 0o111 != 0)
						.unwrap_or(false)
			})
		})
		.unwrap_or(false)
}

/// true when `bin_dir` is already an entry of the current PATH.
fn bin_dir_on_path(bin_dir: &Path) -> bool {
	std::env::var_os("PATH")
		.map(|path| std::env::split_paths(&path).any(|dir| dir == bin_dir))
		.unwrap_or(false)
}

#[cfg(test)]
mod tests {
	use clap::ValueEnum;

	use super::*;

	// serializes env-dependent tests (the process env is global).
	static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

	// restores a set of environment variables to their prior values on drop.
	struct EnvGuard {
		vars: Vec<(&'static str, Option<std::ffi::OsString>)>,
	}

	impl EnvGuard {
		fn save(names: &[&'static str]) -> Self {
			let vars = names
				.iter()
				.map(|name| (*name, std::env::var_os(name)))
				.collect();
			Self { vars }
		}
	}

	impl Drop for EnvGuard {
		fn drop(&mut self) {
			for (name, value) in &self.vars {
				match value {
					Some(value) => unsafe { std::env::set_var(name, value) },
					None => unsafe { std::env::remove_var(name) },
				}
			}
		}
	}

	#[test]
	fn render_targets_requested_bin_name() {
		let bash = String::from_utf8(render(Shell::Bash, "svamm").unwrap()).unwrap();
		assert!(bash.contains("complete -F _svamm"));
		assert!(bash.contains("svamm"));
		let zsh = String::from_utf8(render(Shell::Zsh, "svm").unwrap()).unwrap();
		assert!(zsh.contains("#compdef svm"));
	}

	#[test]
	fn render_is_non_empty_for_every_shell() {
		for shell in Shell::value_variants() {
			let out = String::from_utf8(render(*shell, "svamm").unwrap()).unwrap();
			assert!(!out.trim().is_empty(), "{shell} must render a script");
		}
	}

	#[test]
	fn dynamic_bash_offers_live_names_for_svamm() {
		let bash = String::from_utf8(render(Shell::Bash, "svamm").unwrap()).unwrap();
		assert!(
			bash.contains("_svamm_with_names()"),
			"wrapper must be defined"
		);
		assert!(
			bash.contains("svamm list -q 2>/dev/null"),
			"must call list -q"
		);
		assert!(
			bash.contains(
				"complete -F _svamm_with_names -o nosort -o bashdefault -o default svamm"
			),
			"wrapper must be registered with the static options"
		);
		assert!(
			bash.contains("complete -F _svamm_with_names -o bashdefault -o default svamm"),
			"fallback registration must be present"
		);
		assert!(bash.contains("net|start|stop|restart|rm|run"));
	}

	#[test]
	fn dynamic_bash_targets_alias_binary() {
		let bash = String::from_utf8(render(Shell::Bash, "svm").unwrap()).unwrap();
		assert!(bash.contains("_svm_with_names()"));
		assert!(bash.contains("complete -F _svm_with_names -o bashdefault -o default svm"));
		assert!(bash.contains("svm list -q 2>/dev/null"));
	}

	#[test]
	fn dynamic_zsh_rewrites_only_name_specs() {
		let zsh = String::from_utf8(render(Shell::Zsh, "svamm").unwrap()).unwrap();
		assert_eq!(
			zsh.matches(":name -- Sandbox name:_default").count(),
			0,
			"every name positional must route to the live completer"
		);
		assert_eq!(
			zsh.matches(":name -- Sandbox name:_svamm_names").count(),
			NAME_COMMANDS.len(),
			"exactly the name-taking commands must be rewritten"
		);
		assert!(zsh.contains("_svamm_names()"), "completer must be defined");
		assert!(zsh.contains("svamm list -q 2>/dev/null"));
		assert!(
			zsh.contains(":name -- Sandbox name (slug, no spaces):_default"),
			"create keeps the static spec"
		);
		assert!(
			zsh.contains(
				"*::command -- Command words (trailing arguments, typically after `--`):_default"
			),
			"run trailing command words keep the static spec"
		);
	}

	#[test]
	fn dynamic_zsh_targets_alias_binary() {
		let zsh = String::from_utf8(render(Shell::Zsh, "svm").unwrap()).unwrap();
		assert_eq!(zsh.matches(":name -- Sandbox name:_svm_names").count(), 6);
		assert!(zsh.contains("_svm_names()"));
		assert!(zsh.contains("#compdef svm"));
	}

	#[test]
	fn zsh_dynamic_completer_is_defined_before_first_invocation() {
		// zsh autoload executes the whole file as the function body on the
		// first completion attempt. the dynamic completer must be defined
		// before the generated trailing invocation, or the first completion
		// of a sandbox name fails with `command not found: _<name>_names`.
		for bin_name in BIN_NAMES {
			let zsh = String::from_utf8(render(Shell::Zsh, bin_name).unwrap()).unwrap();
			let entry = zsh
				.find(&format!("_{bin_name}_names() {{"))
				.unwrap_or_else(|| panic!("{bin_name}: dynamic completer must be defined"));
			let guard = zsh
				.find(&format!("if [ \"$funcstack[1]\" = \"_{bin_name}\" ]; then"))
				.unwrap_or_else(|| panic!("{bin_name}: generated invocation guard must exist"));
			assert!(
				entry < guard,
				"{bin_name}: completer must be defined before the generated invocation"
			);
		}
	}

	#[test]
	fn dynamic_fish_offers_live_names() {
		let fish = String::from_utf8(render(Shell::Fish, "svamm").unwrap()).unwrap();
		assert!(fish.contains("__svamm_needs_name"));
		assert!(
			fish.contains(
				"complete -c svamm -n \"__svamm_needs_name\" -a \"(svamm list -q 2>/dev/null)\""
			),
			"fish must register a dynamic source"
		);
		assert!(fish.contains("net start stop restart rm run"));
	}

	#[test]
	fn dynamic_fish_targets_alias_binary() {
		let fish = String::from_utf8(render(Shell::Fish, "svm").unwrap()).unwrap();
		assert!(fish.contains("__svm_needs_name"));
		assert!(fish.contains("-a \"(svm list -q 2>/dev/null)\""));
	}

	#[test]
	fn dynamic_block_skipped_for_other_shells() {
		for shell in [Shell::Elvish, Shell::PowerShell] {
			let out = String::from_utf8(render(shell, "svamm").unwrap()).unwrap();
			assert!(
				!out.contains("list -q"),
				"{shell} must not get a dynamic name block"
			);
		}
	}

	#[test]
	fn file_names_follow_shell_conventions() {
		assert_eq!(Shell::Bash.file_name("svamm"), "svamm.bash");
		assert_eq!(Shell::Zsh.file_name("svamm"), "_svamm");
		assert_eq!(Shell::Fish.file_name("svamm"), "svamm.fish");
	}

	#[test]
	fn detects_shell_from_common_paths() {
		let _env = ENV_LOCK.lock().unwrap();
		let original = std::env::var_os("SHELL");
		for (path, expected) in [
			("/bin/bash", Some(Shell::Bash)),
			("/usr/bin/zsh", Some(Shell::Zsh)),
			("/usr/bin/fish", Some(Shell::Fish)),
			("/usr/bin/tcsh", None),
		] {
			unsafe { std::env::set_var("SHELL", path) };
			assert_eq!(detect_shell(), expected);
		}
		match original {
			Some(value) => unsafe { std::env::set_var("SHELL", value) },
			None => unsafe { std::env::remove_var("SHELL") },
		}
	}

	#[test]
	fn install_writes_zsh_scripts_for_each_bin() {
		let _env = ENV_LOCK.lock().unwrap();
		let home = tempfile::tempdir().unwrap();
		let _guard = EnvGuard::save(&["HOME", "SHELL", "ZDOTDIR", "SVAMM_DRY_RUN"]);
		unsafe { std::env::set_var("HOME", home.path()) };
		unsafe { std::env::set_var("SHELL", "/usr/bin/zsh") };
		unsafe { std::env::remove_var("ZDOTDIR") };
		unsafe { std::env::remove_var("SVAMM_DRY_RUN") };
		install().unwrap();
		let dir = home.path().join(".local/share/zsh/site-functions");
		assert!(dir.join("_svamm").is_file(), "_svamm must be written");
		assert!(dir.join("_svm").is_file(), "_svm must be written");
		let svamm = std::fs::read_to_string(dir.join("_svamm")).unwrap();
		assert!(svamm.contains("#compdef svamm"));
	}

	#[test]
	fn install_honors_dry_run() {
		let _env = ENV_LOCK.lock().unwrap();
		let home = tempfile::tempdir().unwrap();
		let _guard = EnvGuard::save(&["HOME", "SHELL", "SVAMM_DRY_RUN"]);
		unsafe { std::env::set_var("HOME", home.path()) };
		unsafe { std::env::set_var("SHELL", "/bin/bash") };
		unsafe { std::env::set_var("SVAMM_DRY_RUN", "1") };
		install().unwrap();
		let dir = home.path().join(".local/share/bash-completion/completions");
		assert!(
			!dir.join("svamm.bash").exists(),
			"dry run must not write files"
		);
	}

	#[test]
	fn install_skips_when_shell_undetectable() {
		let _env = ENV_LOCK.lock().unwrap();
		let home = tempfile::tempdir().unwrap();
		let _guard = EnvGuard::save(&["HOME", "SHELL"]);
		unsafe { std::env::set_var("HOME", home.path()) };
		unsafe { std::env::set_var("SHELL", "/usr/bin/tcsh") };
		install().unwrap();
		assert!(
			!home.path().join(".local/share/zsh/site-functions").exists(),
			"no scripts should be written for an unknown shell"
		);
	}

	#[test]
	fn ensure_command_links_creates_both_links() {
		let _env = ENV_LOCK.lock().unwrap();
		let home = tempfile::tempdir().unwrap();
		let _guard = EnvGuard::save(&["HOME", "PATH", "SVAMM_BIN_DIR", "SVAMM_DRY_RUN"]);
		unsafe { std::env::set_var("HOME", home.path()) };
		unsafe { std::env::set_var("SVAMM_BIN_DIR", home.path().join("bin")) };
		unsafe { std::env::set_var("PATH", home.path().join("empty-path")) };
		unsafe { std::env::remove_var("SVAMM_DRY_RUN") };
		ensure_command_links().unwrap();
		let bin = home.path().join("bin");
		let exe = std::env::current_exe().unwrap();
		assert_eq!(std::fs::read_link(bin.join("svamm")).unwrap(), exe);
		assert_eq!(std::fs::read_link(bin.join("svm")).unwrap(), exe);
	}

	#[test]
	fn ensure_command_links_leaves_existing_on_path_alone() {
		use std::os::unix::fs::PermissionsExt;

		let _env = ENV_LOCK.lock().unwrap();
		let home = tempfile::tempdir().unwrap();
		let bin = home.path().join("bin");
		std::fs::create_dir_all(&bin).unwrap();
		std::fs::write(bin.join("svm"), b"#!/bin/sh\n").unwrap();
		std::fs::set_permissions(bin.join("svm"), std::fs::Permissions::from_mode(0o755)).unwrap();

		let _guard = EnvGuard::save(&["HOME", "PATH", "SVAMM_BIN_DIR", "SVAMM_DRY_RUN"]);
		unsafe { std::env::set_var("HOME", home.path()) };
		unsafe { std::env::set_var("SVAMM_BIN_DIR", &bin) };
		unsafe { std::env::set_var("PATH", &bin) };
		unsafe { std::env::remove_var("SVAMM_DRY_RUN") };
		ensure_command_links().unwrap();

		assert!(
			bin.join("svm").is_file(),
			"existing svm must not be replaced"
		);
		let exe = std::env::current_exe().unwrap();
		assert_eq!(
			std::fs::read_link(bin.join("svamm")).unwrap(),
			exe,
			"svamm must still be linked"
		);
	}
}
