//! completions (`svamm completions`) contract tests.

mod common;

use common::TestEnv;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[test]
fn zsh_completions_target_svamm() {
	let env = TestEnv::new();
	let stdout = env.run_ok(false, &["completions", "zsh"]);
	assert!(
		stdout.contains("#compdef svamm"),
		"zsh script must define svamm completions"
	);
}

#[test]
fn bash_completions_register_svamm() {
	let env = TestEnv::new();
	let stdout = env.run_ok(false, &["completions", "bash"]);
	assert!(
		stdout.contains("complete -F _svamm"),
		"bash script must register svamm completion"
	);
}

#[test]
fn zsh_completions_for_svm_alias() {
	let env = TestEnv::new();
	let stdout = env.run_ok(false, &["completions", "zsh", "--name", "svm"]);
	assert!(
		stdout.contains("#compdef svm"),
		"zsh script must define svm completions"
	);
}

#[test]
fn fish_completions_are_non_empty() {
	let env = TestEnv::new();
	let stdout = env.run_ok(false, &["completions", "fish"]);
	assert!(!stdout.trim().is_empty(), "fish script must not be empty");
}

#[test]
fn unknown_shell_is_rejected() {
	let env = TestEnv::new();
	let (stderr, _) = env.run_fail(false, &["completions", "tcsh"]);
	assert!(
		stderr.contains("tcsh") || stderr.contains("invalid"),
		"expected a rejection mentioning tcsh; stderr was:\n{stderr}"
	);
}

#[test]
fn help_lists_completions() {
	let env = TestEnv::new();
	let stdout = env.run_ok(false, &["--help"]);
	assert!(
		stdout.split_whitespace().any(|word| word == "completions"),
		"help must list completions"
	);
}

#[test]
fn tree_still_covers_all_commands() {
	let env = TestEnv::new();
	let stdout = env.run_ok(false, &["completions", "bash"]);
	for command in [
		"setup",
		"create",
		"net",
		"restart",
		"rm",
		"prune",
		"completions",
	] {
		assert!(
			stdout.contains(command),
			"completion script must cover {command}"
		);
	}
}

#[test]
fn bash_completions_include_live_name_completion() {
	let env = TestEnv::new();
	let stdout = env.run_ok(false, &["completions", "bash"]);
	assert!(
		stdout.contains("_svamm_with_names"),
		"bash script must define the live-name wrapper"
	);
	assert!(
		stdout.contains("svamm list -q"),
		"bash script must query live sandbox names"
	);
}

#[test]
fn zsh_completions_rewrite_name_positionals_to_live_completer() {
	let env = TestEnv::new();
	let stdout = env.run_ok(false, &["completions", "zsh"]);
	assert!(
		stdout.contains(":name -- Sandbox name:_svamm_names"),
		"zsh script must route name positionals to the live completer"
	);
	assert!(
		stdout.contains("_svamm_names()"),
		"zsh completer must be defined"
	);
	assert!(stdout.contains("svamm list -q"));
}

#[test]
fn fish_completions_include_live_name_completion() {
	let env = TestEnv::new();
	let stdout = env.run_ok(false, &["completions", "fish"]);
	assert!(
		stdout.contains("__svamm_needs_name"),
		"fish script must define the name-position guard"
	);
	assert!(stdout.contains("svamm list -q"));
}

/// write a fake `svamm`/`svm` shim into `dir` that answers `list -q`.
/// the shim returns `names`, or fails when `names` is empty.
fn write_fake_bin(dir: &Path, name: &str, names: &str) {
	let body = if names.is_empty() {
		"exit 1".to_string()
	} else {
		format!("printf '%s\\n' {names}")
	};
	let path = dir.join(name);
	std::fs::write(
		&path,
		format!("#!/bin/sh\nif [ \"$1\" = \"list\" ] && [ \"$2\" = \"-q\" ]; then\n  {body}\nfi\n"),
	)
	.unwrap();
	std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// generate the bash completion for `bin` and drive the live-name wrapper
/// for the synthetic command line `words`. returns the wrapper's stdout.
fn bash_complete(env: &TestEnv, bin: &str, fake_dir: &Path, words: &[&str]) -> String {
	let script = env.run_ok(false, &["completions", "bash", "--name", bin]);
	let script_path = fake_dir.join(format!("{bin}.bash"));
	std::fs::write(&script_path, &script).unwrap();
	let cword = words.len() - 1;
	let cur = words[cword];
	let prev = if cword >= 1 { words[cword - 1] } else { "" };
	let words_bash = words
		.iter()
		.map(|word| format!("'{word}'"))
		.collect::<Vec<_>>()
		.join(" ");
	let driver = format!(
		"source '{p}'\nCOMP_WORDS=({words_bash})\nCOMP_CWORD={cword}\n_{bin}_with_names '{bin}' '{cur}' '{prev}'\nprintf '%s\\n' \"${{COMPREPLY[@]}}\"\n",
		p = script_path.display(),
	);
	let output = std::process::Command::new("bash")
		.arg("-c")
		.arg(&driver)
		.env(
			"PATH",
			format!(
				"{}:{}",
				fake_dir.display(),
				std::env::var("PATH").unwrap_or_default()
			),
		)
		.output()
		.unwrap();
	assert!(
		output.status.success(),
		"bash completion driver failed: stdout:\n{}\nstderr:\n{}",
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr)
	);
	String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn bash_completion_offers_live_sandbox_names() {
	let env = TestEnv::new();
	let dir = tempfile::tempdir().unwrap();
	write_fake_bin(dir.path(), "svamm", "alpha beta");
	let out = bash_complete(&env, "svamm", dir.path(), &["svamm", "start", ""]);
	assert!(
		out.contains("alpha") && out.contains("beta"),
		"name position must offer live names: {out}"
	);
}

#[test]
fn bash_completion_filters_names_by_prefix() {
	let env = TestEnv::new();
	let dir = tempfile::tempdir().unwrap();
	write_fake_bin(dir.path(), "svamm", "alpha beta");
	let out = bash_complete(&env, "svamm", dir.path(), &["svamm", "start", "al"]);
	assert!(
		out.contains("alpha"),
		"matching name must be offered: {out}"
	);
	assert!(
		!out.contains("beta"),
		"non-matching name must be filtered: {out}"
	);
}

#[test]
fn bash_completion_offers_names_after_flags() {
	let env = TestEnv::new();
	let dir = tempfile::tempdir().unwrap();
	write_fake_bin(dir.path(), "svamm", "alpha beta");
	let out = bash_complete(&env, "svamm", dir.path(), &["svamm", "stop", "-a", ""]);
	assert!(
		out.contains("alpha"),
		"names must still be offered after flags: {out}"
	);
}

#[test]
fn bash_completion_offers_names_for_alias_binary() {
	let env = TestEnv::new();
	let dir = tempfile::tempdir().unwrap();
	write_fake_bin(dir.path(), "svm", "gamma");
	let out = bash_complete(&env, "svm", dir.path(), &["svm", "restart", ""]);
	assert!(
		out.contains("gamma"),
		"svm alias must offer live names: {out}"
	);
}

#[test]
fn bash_completion_skips_names_for_create_and_after_name() {
	let env = TestEnv::new();
	let dir = tempfile::tempdir().unwrap();
	write_fake_bin(dir.path(), "svamm", "alpha beta");
	let create = bash_complete(&env, "svamm", dir.path(), &["svamm", "create", ""]);
	assert!(
		!create.contains("alpha"),
		"create must not offer names: {create}"
	);
	assert!(
		create.contains("--replace"),
		"create must keep static completions: {create}"
	);
	let run = bash_complete(&env, "svamm", dir.path(), &["svamm", "run", "alpha", ""]);
	assert!(
		!run.contains("beta"),
		"run must not offer names after a name was given: {run}"
	);
}

#[test]
fn bash_completion_degrades_when_backend_unreachable() {
	let env = TestEnv::new();
	let dir = tempfile::tempdir().unwrap();
	write_fake_bin(dir.path(), "svamm", "");
	let out = bash_complete(&env, "svamm", dir.path(), &["svamm", "start", ""]);
	assert!(
		!out.contains("alpha"),
		"no names expected when list -q fails: {out}"
	);
	assert!(
		out.contains("-h"),
		"static flags must still be offered as a fallback: {out}"
	);
}
