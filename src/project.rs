//! project `svamm.json` reading and resolution.
//!
//! a project file in the main rw mount can declare a per-project image
//! override and network rules merged with the base policy. the module only
//! reads and resolves project settings at a host path. reconciling those
//! settings onto a live sandbox's config lives in [`crate::reconcile`].

use std::collections::BTreeMap;
use std::path::Path;

use crate::assets;
use crate::config::Config;
use crate::netrules;
use crate::paths;

/// read the `image` reference from a project `svamm.json`, if present.
pub fn project_image(path: &Path) -> Result<Option<String>, String> {
	let text = std::fs::read_to_string(path)
		.map_err(|err| format!("failed to read {}: {err}", path.display()))?;
	let data: serde_json::Value = serde_json::from_str(&text)
		.map_err(|err| format!("invalid JSON in {}: {err}", path.display()))?;
	let Some(image) = data.get("image") else {
		return Ok(None);
	};
	let Some(reference) = image.as_str() else {
		return Err(format!(
			"{}: \"image\" must be a string image reference",
			path.display()
		));
	};
	let reference = reference.trim();
	if reference.is_empty() {
		return Ok(None);
	}
	Ok(Some(reference.to_string()))
}

/// merge base net-rules with a project svamm.json.
///
/// the rule that reaches the local registry (`host:tcp:<proxy_port>`) is
/// generated from the configured read-only proxy port, so registry access
/// does not depend on the net-rules file.
pub fn merge_net_rules(
	config: &Config,
	project: Option<&Path>,
) -> anyhow::Result<netrules::MergedRules> {
	let base = paths::expand_tilde(&config.configs.net_rules);
	assets::ensure_one("net-rules.json", &base);
	let mut merged = netrules::merge(&base, project).map_err(anyhow::Error::msg)?;
	let registry_rule = format!("host:tcp:{}", config.registry.proxy_port);
	if !merged.allow.contains(&registry_rule) {
		merged.allow.push(registry_rule);
	}
	Ok(merged)
}

/// the environment variable the vm image reads at boot so docker inside the
/// sandbox can reach the local registry proxy. always present and always
/// re-applied, so a declared project key cannot drop or override it.
pub(crate) const REGISTRY_PROXY_ENV: &str = "SVAMM_REGISTRY_PROXY_PORT";

/// true when `name` is a shell-safe environment variable name.
fn is_env_name(name: &str) -> bool {
	let mut bytes = name.bytes();
	match bytes.next() {
		Some(first) if first == b'_' || first.is_ascii_alphabetic() => {}
		_ => return false,
	}
	bytes.all(|b| b == b'_' || b.is_ascii_alphanumeric())
}

/// read the `envs` object from a project `svamm.json`, if present.
///
/// `"envs"` declares guest environment variables. every key must be a
/// shell-safe variable name; every value of the exact form `${NAME}` is
/// resolved from the host environment when svamm runs.
pub fn project_envs(path: &Path) -> Result<Option<BTreeMap<String, String>>, String> {
	let text = std::fs::read_to_string(path)
		.map_err(|err| format!("failed to read {}: {err}", path.display()))?;
	let data: serde_json::Value = serde_json::from_str(&text)
		.map_err(|err| format!("invalid JSON in {}: {err}", path.display()))?;
	let Some(envs) = data.get("envs") else {
		return Ok(None);
	};
	let Some(envs) = envs.as_object() else {
		return Err(format!(
			"{}: \"envs\" must be an object of \"NAME\": \"value\" entries",
			path.display()
		));
	};
	let mut out = BTreeMap::new();
	for (name, value) in envs {
		if !is_env_name(name) {
			return Err(format!(
				"{}: \"envs.{name}\" must be a valid variable name ([A-Za-z_][A-Za-z0-9_]*)",
				path.display()
			));
		}
		let Some(value) = value.as_str() else {
			return Err(format!(
				"{}: \"envs.{name}\" must be a string value",
				path.display()
			));
		};
		out.insert(name.clone(), value.to_string());
	}
	Ok(Some(out))
}

/// true when `value` is an exact `${NAME}` host environment reference.
pub(crate) fn is_host_env_reference(value: &str) -> bool {
	match value.strip_prefix("${") {
		Some(rest) => rest
			.strip_suffix('}')
			.is_some_and(|name| !name.is_empty() && !name.contains('}')),
		None => false,
	}
}

/// resolve a `"${NAME}"` value from the host environment.
///
/// only the exact `${NAME}` form resolves. a name outside the allowed
/// characters, or an unset or empty host variable, fails loudly. every other
/// value passes through verbatim.
fn resolve_env_value(key: &str, value: &str) -> Result<String, String> {
	if !is_host_env_reference(value) {
		return Ok(value.to_string());
	}
	let name = &value[2..value.len() - 1];
	if !is_env_name(name) {
		return Err(format!(
			"envs.{key}: invalid host variable reference {value}: \
			 name '{name}' must be [A-Za-z_][A-Za-z0-9_]*"
		));
	}
	match std::env::var(name) {
		Ok(resolved) if !resolved.is_empty() => Ok(resolved),
		_ => Err(format!(
			"envs.{key}: host variable {name} is unset or empty (referenced as {value})"
		)),
	}
}

/// the env list a sandbox boots with when created: the generated registry
/// proxy var first, then any project `envs` in key order.
pub(crate) fn desired_env(config: &Config, project: Option<&Path>) -> Result<Vec<String>, String> {
	if let Some(project) = project
		&& let Some(envs) = project_envs(project)?
	{
		return desired_env_entries(config, &envs);
	}
	Ok(vec![format!(
		"{REGISTRY_PROXY_ENV}={}",
		config.registry.proxy_port
	)])
}

/// the env list for the generated registry proxy var plus the given declared
/// envs. a declared key never overrides the generated proxy var.
pub(crate) fn desired_env_entries(
	config: &Config,
	envs: &BTreeMap<String, String>,
) -> Result<Vec<String>, String> {
	let mut env = vec![format!(
		"{REGISTRY_PROXY_ENV}={}",
		config.registry.proxy_port
	)];
	for (key, value) in envs {
		if key == REGISTRY_PROXY_ENV {
			continue;
		}
		env.push(format!("{key}={}", resolve_env_value(key, value.as_str())?));
	}
	Ok(env)
}

/// the shared empty file mounted as an overlay over a project's `svamm.json`
/// inside each sandbox.
///
/// the single-file mount is the only mount kind microsandbox accepts at a
/// guest file path, so masking with an empty readonly facade keeps the real
/// host config out of guest processes (they cannot read or write it).
pub fn mask_file() -> std::path::PathBuf {
	paths::state_dir().join("svamm.json.mask")
}

/// create (or truncate) the mask file.
///
/// a missing mask would make the persisted overlay host unclassifiable as a
/// file, so a real start recreates it before anything references it.
pub fn ensure_mask_file() -> anyhow::Result<()> {
	let path = mask_file();
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent)?;
	}
	std::fs::File::create(&path)?;
	Ok(())
}

/// how a project's `svamm.json` appears inside its sandboxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvammJsonVisibility {
	/// an empty read-only facade: guest processes cannot read the real config.
	Masked,
	/// the real file mounted read-only: guest processes can read it.
	Readonly,
}

/// read the `svamm_json` visibility mode from a project file.
///
/// the key is absent -> `Masked` (the default). an unknown value fails loudly
/// so a typo cannot silently change the overlay.
pub fn svamm_json_visibility(path: &Path) -> Result<SvammJsonVisibility, String> {
	let text = std::fs::read_to_string(path)
		.map_err(|err| format!("failed to read {}: {err}", path.display()))?;
	let data: serde_json::Value = serde_json::from_str(&text)
		.map_err(|err| format!("invalid JSON in {}: {err}", path.display()))?;
	match data.get("svamm_json").and_then(serde_json::Value::as_str) {
		Some("masked") => Ok(SvammJsonVisibility::Masked),
		Some("readonly") => Ok(SvammJsonVisibility::Readonly),
		Some(_) => Err(format!(
			"{}: \"svamm_json\" must be \"masked\" or \"readonly\"",
			path.display()
		)),
		None => Ok(SvammJsonVisibility::Masked),
	}
}

/// the host path the svamm.json overlay binds for a resolved mode: the shared
/// empty mask, or the real `svamm.json` for guest-readable projects.
pub fn svamm_json_overlay_host(project: &Path, visibility: SvammJsonVisibility) -> String {
	match visibility {
		SvammJsonVisibility::Masked => mask_file().display().to_string(),
		SvammJsonVisibility::Readonly => project.display().to_string(),
	}
}

/// resolve the sandbox image reference for a project.
/// an override must be a valid OCI reference.
pub fn resolve_sandbox_image_ref(
	config: &Config,
	project: Option<&Path>,
) -> anyhow::Result<String> {
	let Some(project) = project else {
		return Ok(config.sandbox_image_ref());
	};
	let Some(image) = project_image(project).map_err(anyhow::Error::msg)? else {
		return Ok(config.sandbox_image_ref());
	};
	image
		.parse::<microsandbox_image::Reference>()
		.map_err(|err| {
			anyhow::anyhow!(
				"{}: invalid image reference '{image}': {err}",
				project.display()
			)
		})?;
	Ok(image)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn write_json(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
		let path = dir.join(name);
		std::fs::write(&path, content).unwrap();
		path
	}

	#[test]
	fn project_image_absent_returns_none() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(dir.path(), "svamm.json", r#"{"network": {"allow": []}}"#);
		assert_eq!(project_image(&file).unwrap(), None);
	}

	#[test]
	fn project_image_parses_reference() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(
			dir.path(),
			"svamm.json",
			r#"{"image": "127.0.0.1:50000/acme:latest"}"#,
		);
		assert_eq!(
			project_image(&file).unwrap(),
			Some("127.0.0.1:50000/acme:latest".to_string())
		);
	}

	#[test]
	fn project_image_empty_is_unset() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(dir.path(), "svamm.json", r#"{"image": "  "}"#);
		assert_eq!(project_image(&file).unwrap(), None);
	}

	#[test]
	fn project_image_rejects_non_string() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(
			dir.path(),
			"svamm.json",
			r#"{"image": {"reference": "127.0.0.1:50000/acme:latest"}}"#,
		);
		assert!(
			project_image(&file)
				.unwrap_err()
				.contains("\"image\" must be a string")
		);
	}

	#[test]
	fn project_image_rejects_bad_json() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(dir.path(), "svamm.json", "not json");
		assert!(project_image(&file).unwrap_err().contains("invalid JSON"));
	}

	#[test]
	fn resolve_sandbox_image_ref_without_project_uses_default() {
		let config = Config::default();
		assert_eq!(
			resolve_sandbox_image_ref(&config, None).unwrap(),
			config.sandbox_image_ref()
		);
	}

	#[test]
	fn resolve_sandbox_image_ref_uses_project_override() {
		let config = Config::default();
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(
			dir.path(),
			"svamm.json",
			r#"{"image": "127.0.0.1:50000/acme:latest"}"#,
		);
		assert_eq!(
			resolve_sandbox_image_ref(&config, Some(&file)).unwrap(),
			"127.0.0.1:50000/acme:latest"
		);
	}

	#[test]
	fn resolve_sandbox_image_ref_rejects_invalid_reference() {
		let config = Config::default();
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(dir.path(), "svamm.json", r#"{"image": "not a ref!!"}"#);
		let err = resolve_sandbox_image_ref(&config, Some(&file)).unwrap_err();
		assert!(
			err.to_string()
				.contains("invalid image reference 'not a ref!!'"),
			"unexpected error: {err}"
		);
	}

	#[test]
	fn svamm_json_visibility_defaults_to_masked() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(dir.path(), "svamm.json", r#"{"network": {"allow": []}}"#);
		assert_eq!(
			svamm_json_visibility(&file).unwrap(),
			SvammJsonVisibility::Masked
		);
	}

	#[test]
	fn svamm_json_visibility_parses_both_modes() {
		let dir = tempfile::tempdir().unwrap();
		let masked = write_json(dir.path(), "masked.json", r#"{"svamm_json": "masked"}"#);
		let readonly = write_json(dir.path(), "readonly.json", r#"{"svamm_json": "readonly"}"#);
		assert_eq!(
			svamm_json_visibility(&masked).unwrap(),
			SvammJsonVisibility::Masked
		);
		assert_eq!(
			svamm_json_visibility(&readonly).unwrap(),
			SvammJsonVisibility::Readonly
		);
	}

	#[test]
	fn svamm_json_visibility_rejects_unknown_value() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(dir.path(), "svamm.json", r#"{"svamm_json": "hidden"}"#);
		let err = svamm_json_visibility(&file).unwrap_err();
		assert!(
			err.contains("\"svamm_json\" must be \"masked\" or \"readonly\""),
			"unexpected error: {err}"
		);
	}

	#[test]
	fn svamm_json_overlay_host_follows_the_mode() {
		let dir = tempfile::tempdir().unwrap();
		let project = write_json(dir.path(), "svamm.json", "{}");
		assert_eq!(
			svamm_json_overlay_host(&project, SvammJsonVisibility::Masked),
			mask_file().display().to_string()
		);
		assert_eq!(
			svamm_json_overlay_host(&project, SvammJsonVisibility::Readonly),
			project.display().to_string()
		);
	}

	// serialize tests that mutate the process env.
	static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

	#[test]
	fn project_envs_absent_returns_none() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(dir.path(), "svamm.json", r#"{"network": {"allow": []}}"#);
		assert_eq!(project_envs(&file).unwrap(), None);
	}

	#[test]
	fn project_envs_parses_object() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(
			dir.path(),
			"svamm.json",
			r#"{"envs": {"MY_KEY": "value", "MY_HOST_REF": "${HOST_VAR}"}}"#,
		);
		let envs = project_envs(&file).unwrap().unwrap();
		assert_eq!(envs.get("MY_KEY").map(String::as_str), Some("value"));
		assert_eq!(
			envs.get("MY_HOST_REF").map(String::as_str),
			Some("${HOST_VAR}")
		);
	}

	#[test]
	fn project_envs_rejects_non_object() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(dir.path(), "svamm.json", r#"{"envs": ["A=1"]}"#);
		let err = project_envs(&file).unwrap_err();
		assert!(
			err.contains("\"envs\" must be an object"),
			"unexpected error: {err}"
		);
	}

	#[test]
	fn project_envs_rejects_non_string_value() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(dir.path(), "svamm.json", r#"{"envs": {"MY_KEY": 42}}"#);
		let err = project_envs(&file).unwrap_err();
		assert!(
			err.contains("\"envs.MY_KEY\" must be a string value"),
			"unexpected error: {err}"
		);
	}

	#[test]
	fn project_envs_rejects_invalid_names() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(dir.path(), "svamm.json", r#"{"envs": {"1BAD": "value"}}"#);
		let err = project_envs(&file).unwrap_err();
		assert!(
			err.contains("\"envs.1BAD\" must be a valid variable name"),
			"unexpected error: {err}"
		);
	}

	#[test]
	fn resolve_env_value_passes_literals_through() {
		assert_eq!(resolve_env_value("A", "plain").unwrap(), "plain");
		assert_eq!(
			resolve_env_value("A", "with=${x}inside").unwrap(),
			"with=${x}inside"
		);
		assert_eq!(resolve_env_value("A", "${B}C").unwrap(), "${B}C");
		assert_eq!(resolve_env_value("A", "${}").unwrap(), "${}");
		assert_eq!(resolve_env_value("A", "").unwrap(), "");
	}

	#[test]
	fn resolve_env_value_resolves_host_variable() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original = std::env::var_os("SVAMM_TEST_RESOLVE_VAR");
		unsafe { std::env::set_var("SVAMM_TEST_RESOLVE_VAR", "resolved") };
		assert_eq!(
			resolve_env_value("A", "${SVAMM_TEST_RESOLVE_VAR}").unwrap(),
			"resolved"
		);
		match original {
			Some(value) => unsafe { std::env::set_var("SVAMM_TEST_RESOLVE_VAR", value) },
			None => unsafe { std::env::remove_var("SVAMM_TEST_RESOLVE_VAR") },
		}
	}

	#[test]
	fn resolve_env_value_fails_on_unset_or_empty_host_variable() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original = std::env::var_os("SVAMM_TEST_UNSET_VAR");
		unsafe { std::env::remove_var("SVAMM_TEST_UNSET_VAR") };
		let err = resolve_env_value("SECRET", "${SVAMM_TEST_UNSET_VAR}").unwrap_err();
		assert!(
			err.contains("envs.SECRET: host variable SVAMM_TEST_UNSET_VAR is unset or empty"),
			"unexpected error: {err}"
		);
		unsafe { std::env::set_var("SVAMM_TEST_UNSET_VAR", "") };
		let err = resolve_env_value("SECRET", "${SVAMM_TEST_UNSET_VAR}").unwrap_err();
		assert!(
			err.contains("host variable SVAMM_TEST_UNSET_VAR is unset or empty"),
			"unexpected error: {err}"
		);
		match original {
			Some(value) => unsafe { std::env::set_var("SVAMM_TEST_UNSET_VAR", value) },
			None => unsafe { std::env::remove_var("SVAMM_TEST_UNSET_VAR") },
		}
	}

	#[test]
	fn resolve_env_value_fails_on_invalid_host_variable_name() {
		for (value, name) in [
			("${MY-VAR}", "MY-VAR"),
			("${1BAD}", "1BAD"),
			("${A.B}", "A.B"),
			("${A B}", "A B"),
		] {
			let err = resolve_env_value("SECRET", value).unwrap_err();
			assert!(
				err.contains(&format!(
					"invalid host variable reference {value}: name '{name}'"
				)),
				"unexpected error for {value}: {err}"
			);
			assert!(
				err.contains("[A-Za-z_][A-Za-z0-9_]*"),
				"unexpected error for {value}: {err}"
			);
		}
	}

	#[test]
	fn is_host_env_reference_accepts_the_exact_form_only() {
		assert!(is_host_env_reference("${NAME}"));
		assert!(is_host_env_reference("${_WITH_UNDERSCORE}"));
		assert!(!is_host_env_reference("plain"));
		assert!(!is_host_env_reference(""));
		assert!(!is_host_env_reference("${}"));
		assert!(!is_host_env_reference("${A}B"));
		assert!(!is_host_env_reference("${A}${B}"));
		assert!(!is_host_env_reference("pre-${A}"));
		assert!(!is_host_env_reference("${A}B}"));
	}

	fn with_proxy_port(port: u16) -> (u16, Option<std::ffi::OsString>) {
		let original = std::env::var_os("SVAMM_REGISTRY_PROXY_PORT");
		unsafe { std::env::set_var("SVAMM_REGISTRY_PROXY_PORT", port.to_string()) };
		(port, original)
	}

	fn restore_var(name: &str, original: Option<std::ffi::OsString>) {
		match original {
			Some(value) => unsafe { std::env::set_var(name, value) },
			None => unsafe { std::env::remove_var(name) },
		}
	}

	#[test]
	fn desired_env_without_project_has_proxy_var_only() {
		let _guard = ENV_LOCK.lock().unwrap();
		let (port, original) = with_proxy_port(50021);
		let env = desired_env(&Config::default(), None).unwrap();
		assert_eq!(env, vec![format!("SVAMM_REGISTRY_PROXY_PORT={port}")]);
		restore_var("SVAMM_REGISTRY_PROXY_PORT", original);
	}

	#[test]
	fn desired_env_merges_project_envs_after_proxy_var() {
		let _guard = ENV_LOCK.lock().unwrap();
		let (port, original) = with_proxy_port(50022);
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(
			dir.path(),
			"svamm.json",
			r#"{"envs": {"B": "two", "A": "one"}}"#,
		);
		let env = desired_env(&Config::default(), Some(&file)).unwrap();
		assert_eq!(
			env,
			vec![
				format!("SVAMM_REGISTRY_PROXY_PORT={port}"),
				"A=one".to_string(),
				"B=two".to_string(),
			]
		);
		restore_var("SVAMM_REGISTRY_PROXY_PORT", original);
	}

	#[test]
	fn desired_env_skips_declared_proxy_key() {
		let _guard = ENV_LOCK.lock().unwrap();
		let (port, original) = with_proxy_port(50023);
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(
			dir.path(),
			"svamm.json",
			r#"{"envs": {"SVAMM_REGISTRY_PROXY_PORT": "12345", "A": "one"}}"#,
		);
		let env = desired_env(&Config::default(), Some(&file)).unwrap();
		assert_eq!(
			env,
			vec![
				format!("SVAMM_REGISTRY_PROXY_PORT={port}"),
				"A=one".to_string(),
			]
		);
		restore_var("SVAMM_REGISTRY_PROXY_PORT", original);
	}

	#[test]
	fn desired_env_resolves_and_fails_on_unset_host_reference() {
		let _guard = ENV_LOCK.lock().unwrap();
		let (_, original) = with_proxy_port(50024);
		let original_host = std::env::var_os("SVAMM_TEST_DESIRED_VAR");
		unsafe { std::env::set_var("SVAMM_TEST_DESIRED_VAR", "secret") };
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(
			dir.path(),
			"svamm.json",
			r#"{"envs": {"SECRET": "${SVAMM_TEST_DESIRED_VAR}"}}"#,
		);
		let env = desired_env(&Config::default(), Some(&file)).unwrap();
		assert!(env.contains(&"SECRET=secret".to_string()), "env: {env:?}");
		unsafe { std::env::remove_var("SVAMM_TEST_DESIRED_VAR") };
		let err = desired_env(&Config::default(), Some(&file)).unwrap_err();
		assert!(
			err.contains("host variable SVAMM_TEST_DESIRED_VAR is unset"),
			"unexpected error: {err}"
		);
		match original_host {
			Some(value) => unsafe { std::env::set_var("SVAMM_TEST_DESIRED_VAR", value) },
			None => unsafe { std::env::remove_var("SVAMM_TEST_DESIRED_VAR") },
		}
		restore_var("SVAMM_REGISTRY_PROXY_PORT", original);
	}
}
