//! `svamm start`/`restart` env reconciliation tests.
//!
//! A project `svamm.json` `envs` object declares guest environment variables.
//! Starting a sandbox re-evaluates the declared envs, resolves `${NAME}`
//! references from the host environment, and rewrites the durable `env` array
//! so the change applies on the next boot, without recreating the vm.
//! A project without an `envs` object leaves the persisted env untouched.

mod common;

use common::{DIGEST_B, TestEnv};

/// Build the seeded config for a flat sandbox whose main rw mount is `proj`
/// (which must already contain a `svamm.json`) and whose durable env starts
/// as `env_entries`.
fn seed_config(
	env: &TestEnv,
	proj: &std::path::Path,
	env_entries: serde_json::Value,
) -> serde_json::Value {
	serde_json::json!({
		"name": "alpha",
		"image": {"Oci": {
			"reference": format!("127.0.0.1:{}/svamm:latest", env.port),
			"root_disk": {"kind": "flat", "size_mib": 10240, "fstype": "ext4", "clone": "auto"}
		}},
		"manifest_digest": DIGEST_B,
		"runtime": {"workdir": "/home/agent/proj", "shell": "/bin/sh", "scripts": {}},
		"labels": {"svamm": "managed"},
		"env": env_entries,
		"rlimits": [],
		"mounts": [
			{
				"type": "Bind", "host": proj.display().to_string(), "guest": "/home/agent/proj",
				"options": {"readonly": false, "noexec": false, "nosuid": false, "nodev": false},
				"stat_virtualization": "strict", "host_permissions": "private",
				"follow_root_symlinks": false, "quota_mib": 32768
			}
		],
		"pull_policy": "IfMissing",
		"security_profile": "default",
		"deployment_profile": "single_tenant",
		"lifecycle": {"ephemeral": false, "max_duration_secs": null, "idle_timeout_secs": null},
	})
}

/// the env of a freshly created sandbox: only the generated proxy var, in
/// the durable `EnvVar` object shape.
fn proxy_only(env: &TestEnv) -> serde_json::Value {
	serde_json::json!([
		{"key": "SVAMM_REGISTRY_PROXY_PORT", "value": env.proxy_port.to_string()}
	])
}

/// fetch the persisted env array for `name` from the seeded database as raw JSON.
async fn persisted_env_value(env: &TestEnv, name: &str) -> serde_json::Value {
	let db = env.db().await;
	let config: String =
		sqlx::query_scalar::<_, String>("SELECT config FROM sandbox WHERE name = ?")
			.bind(name)
			.fetch_one(&db)
			.await
			.unwrap();
	let value: serde_json::Value = serde_json::from_str(&config).unwrap();
	value
		.get("env")
		.cloned()
		.unwrap_or_else(|| serde_json::Value::Array(vec![]))
}

/// fetch the persisted env array for `name` from the seeded database as
/// `KEY=VALUE` strings from its `EnvVar` objects.
async fn persisted_env(env: &TestEnv, name: &str) -> Vec<String> {
	let value = persisted_env_value(env, name).await;
	value
		.as_array()
		.map(|entries| {
			entries
				.iter()
				.filter_map(|entry| {
					let key = entry.get("key")?.as_str()?;
					let value = entry.get("value")?.as_str()?;
					Some(format!("{key}={value}"))
				})
				.collect()
		})
		.unwrap_or_default()
}

#[tokio::test]
async fn start_previews_project_env_change_in_dry_run() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"envs": {"MY_KEY": "value"}}"#,
	)
	.unwrap();
	env.seed_sandbox(
		"alpha",
		"Stopped",
		Some(("svamm", "managed")),
		seed_config(&env, proj.path(), proxy_only(&env)),
	)
	.await;

	let stdout = env.run_ok(true, &["start", "alpha"]);
	assert!(
		stdout.contains("apply env alpha"),
		"dry-run must preview env changes; stdout:\n{stdout}"
	);
	assert!(
		stdout.contains("+ MY_KEY=value"),
		"added env var must be previewed as an addition; stdout:\n{stdout}"
	);
	assert!(
		!stdout.contains("- MY_KEY"),
		"nothing is removed in this change; stdout:\n{stdout}"
	);
}

#[tokio::test]
async fn start_previews_host_interpolation_in_dry_run() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"envs": {"SECRET": "${SVAMM_TEST_IT_HOST_VAR}"}}"#,
	)
	.unwrap();
	env.seed_sandbox(
		"alpha",
		"Stopped",
		Some(("svamm", "managed")),
		seed_config(&env, proj.path(), proxy_only(&env)),
	)
	.await;

	let output = env
		.cmd(true)
		.env("SVAMM_TEST_IT_HOST_VAR", "resolved-secret")
		.args(["start", "alpha"])
		.output()
		.unwrap();
	assert!(output.status.success());
	let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
	assert!(
		stdout.contains("+ SECRET"),
		"host interpolation must resolve at reconcile time; stdout:\n{stdout}"
	);
	assert!(
		!stdout.contains("resolved-secret"),
		"interpolated values must be hidden in the diff; stdout:\n{stdout}"
	);
}

#[tokio::test]
async fn start_persists_env_changes_and_re_resolves_on_restart() {
	let env = TestEnv::new();
	env.init_db().await;
	env.seed_registry().await;
	env.seed_vmdk_file(DIGEST_B).await;
	let proj = tempfile::tempdir().unwrap();
	let project_file = proj.path().join("svamm.json");
	std::fs::write(
		&project_file,
		r#"{"envs": {"MY_KEY": "value", "SECRET": "${SVAMM_TEST_IT_HOST_VAR}"}}"#,
	)
	.unwrap();
	env.seed_sandbox(
		"alpha",
		"Stopped",
		Some(("svamm", "managed")),
		seed_config(&env, proj.path(), proxy_only(&env)),
	)
	.await;

	// The boot fails (no KVM/msb in this environment); the durable config is
	// what this test verifies.
	let _ = env
		.cmd(false)
		.env("SVAMM_TEST_IT_HOST_VAR", "secret-one")
		.args(["start", "alpha"])
		.output()
		.unwrap();
	assert_eq!(
		persisted_env(&env, "alpha").await,
		vec![
			format!("SVAMM_REGISTRY_PROXY_PORT={}", env.proxy_port),
			"MY_KEY=value".to_string(),
			"SECRET=secret-one".to_string(),
		]
	);
	// the durable `env` field must be `EnvVar` objects, never `KEY=VALUE`
	// strings: the runtime refuses to deserialize the string form.
	let persisted_value = persisted_env_value(&env, "alpha").await;
	assert!(
		persisted_value
			.as_array()
			.unwrap()
			.iter()
			.all(|entry| entry.is_object()),
		"env entries must persist as EnvVar objects: {persisted_value}"
	);
	assert_eq!(
		persisted_value,
		serde_json::json!([
			{"key": "SVAMM_REGISTRY_PROXY_PORT", "value": env.proxy_port.to_string()},
			{"key": "MY_KEY", "value": "value"},
			{"key": "SECRET", "value": "secret-one"},
		])
	);

	// a changed value and a changed host variable re-apply on the next restart.
	std::fs::write(
		&project_file,
		r#"{"envs": {"MY_KEY": "new-value", "SECRET": "${SVAMM_TEST_IT_HOST_VAR}"}}"#,
	)
	.unwrap();
	let _ = env
		.cmd(false)
		.env("SVAMM_TEST_IT_HOST_VAR", "secret-two")
		.args(["restart", "alpha"])
		.output()
		.unwrap();
	assert_eq!(
		persisted_env(&env, "alpha").await,
		vec![
			format!("SVAMM_REGISTRY_PROXY_PORT={}", env.proxy_port),
			"MY_KEY=new-value".to_string(),
			"SECRET=secret-two".to_string(),
		]
	);

	// removing the `envs` key drops every key svamm applied on the next
	// restart: a missing declaration is equivalent to an empty one.
	std::fs::write(&project_file, "{}").unwrap();
	let _ = env.cmd(false).args(["restart", "alpha"]).output().unwrap();
	assert_eq!(
		persisted_env(&env, "alpha").await,
		vec![format!("SVAMM_REGISTRY_PROXY_PORT={}", env.proxy_port)],
		"a missing envs key must drop the previously applied keys"
	);

	// an explicit empty `envs` object keeps them dropped (idempotent).
	std::fs::write(&project_file, r#"{"envs": {}}"#).unwrap();
	let _ = env.cmd(false).args(["restart", "alpha"]).output().unwrap();
	assert_eq!(
		persisted_env(&env, "alpha").await,
		vec![format!("SVAMM_REGISTRY_PROXY_PORT={}", env.proxy_port)],
		"an empty envs object must keep the declared keys dropped"
	);
}

#[tokio::test]
async fn start_fails_loudly_on_unset_host_env() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"envs": {"SECRET": "${SVAMM_DEFINITELY_UNSET_VAR_XYZ}"}}"#,
	)
	.unwrap();
	env.seed_sandbox(
		"alpha",
		"Stopped",
		Some(("svamm", "managed")),
		seed_config(&env, proj.path(), proxy_only(&env)),
	)
	.await;

	let (stderr, _) = env.run_fail(true, &["start", "alpha"]);
	assert!(
		stderr.contains(
			"envs.SECRET: host variable SVAMM_DEFINITELY_UNSET_VAR_XYZ is unset or empty"
		),
		"expected a loud validation error; stderr:\n{stderr}"
	);
}

#[tokio::test]
async fn start_fails_loudly_on_invalid_host_env_reference() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"envs": {"SECRET": "${MY-VAR}"}}"#,
	)
	.unwrap();
	env.seed_sandbox(
		"alpha",
		"Stopped",
		Some(("svamm", "managed")),
		seed_config(&env, proj.path(), proxy_only(&env)),
	)
	.await;

	let (stderr, _) = env.run_fail(true, &["start", "alpha"]);
	assert!(
		stderr.contains(
			"envs.SECRET: invalid host variable reference ${MY-VAR}: name 'MY-VAR' \
			 must be [A-Za-z_][A-Za-z0-9_]*"
		),
		"expected a loud validation error; stderr:\n{stderr}"
	);
}

#[tokio::test]
async fn declared_proxy_key_cannot_override_or_duplicate() {
	let env = TestEnv::new();
	env.init_db().await;
	env.seed_registry().await;
	env.seed_vmdk_file(DIGEST_B).await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"envs": {"SVAMM_REGISTRY_PROXY_PORT": "12345", "MY_KEY": "value"}}"#,
	)
	.unwrap();
	env.seed_sandbox(
		"alpha",
		"Stopped",
		Some(("svamm", "managed")),
		seed_config(&env, proj.path(), proxy_only(&env)),
	)
	.await;

	let _ = env.cmd(false).args(["start", "alpha"]).output().unwrap();
	assert_eq!(
		persisted_env(&env, "alpha").await,
		vec![
			format!("SVAMM_REGISTRY_PROXY_PORT={}", env.proxy_port),
			"MY_KEY=value".to_string(),
		],
		"the generated proxy var must win and never duplicate"
	);
}

#[tokio::test]
async fn start_hides_interpolated_values_and_shows_literals_in_the_diff() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"envs": {"SECRET": "${SVAMM_TEST_IT_HOST_VAR}", "LITERAL": "plain"}}"#,
	)
	.unwrap();
	env.seed_sandbox(
		"alpha",
		"Stopped",
		Some(("svamm", "managed")),
		seed_config(
			&env,
			proj.path(),
			serde_json::json!([
				{"key": "SVAMM_REGISTRY_PROXY_PORT", "value": env.proxy_port.to_string()},
				{"key": "SECRET", "value": "old-secret"},
				{"key": "LITERAL", "value": "old-plain"},
			]),
		),
	)
	.await;

	let output = env
		.cmd(true)
		.env("SVAMM_TEST_IT_HOST_VAR", "new-secret")
		.args(["start", "alpha"])
		.output()
		.unwrap();
	assert!(output.status.success());
	let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
	assert!(
		stdout.contains("- SECRET"),
		"removed secret value must be hidden; stdout:\n{stdout}"
	);
	assert!(
		stdout.contains("+ SECRET"),
		"added secret value must be hidden; stdout:\n{stdout}"
	);
	assert!(
		stdout.contains("- LITERAL=old-plain"),
		"literal changes stay fully visible; stdout:\n{stdout}"
	);
	assert!(
		stdout.contains("+ LITERAL=plain"),
		"literal changes stay fully visible; stdout:\n{stdout}"
	);
	assert!(
		!stdout.contains("old-secret") && !stdout.contains("new-secret"),
		"resolved secret values must never appear; stdout:\n{stdout}"
	);
}

#[tokio::test]
async fn start_skips_env_reconcile_when_envs_absent() {
	// a sandbox whose env came from the base config only must not be touched
	// when the project declares no `envs` object and svamm never managed it.
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"network": {"allow": []}}"#,
	)
	.unwrap();
	let base_env = serde_json::json!([
		{"key": "PATH", "value": "/home/agent/.cargo/bin:/usr/bin"},
		{"key": "SVAMM_REGISTRY_PROXY_PORT", "value": "50001"},
	]);
	env.seed_sandbox(
		"alpha",
		"Stopped",
		Some(("svamm", "managed")),
		seed_config(&env, proj.path(), base_env.clone()),
	)
	.await;

	let _ = env.cmd(false).args(["start", "alpha"]).output().unwrap();
	assert_eq!(
		persisted_env_value(&env, "alpha").await,
		base_env,
		"an undeclared env with no svamm history must be left untouched"
	);
}

#[tokio::test]
async fn start_preserves_foreign_env_entries_and_drops_removed_project_keys() {
	let env = TestEnv::new();
	env.init_db().await;
	env.seed_registry().await;
	env.seed_vmdk_file(DIGEST_B).await;
	let proj = tempfile::tempdir().unwrap();
	let project_file = proj.path().join("svamm.json");
	// a sandbox whose env also carries base/image entries (PATH, LANG, TERM)
	// that svamm does not manage.
	let base_env = serde_json::json!([
		{"key": "PATH", "value": "/home/agent/.cargo/bin:/usr/bin"},
		{"key": "LANG", "value": "en_US.UTF-8"},
		{"key": "TERM", "value": "xterm-256color"},
		{"key": "SVAMM_REGISTRY_PROXY_PORT", "value": env.proxy_port.to_string()},
	]);
	env.seed_sandbox(
		"alpha",
		"Stopped",
		Some(("svamm", "managed")),
		seed_config(&env, proj.path(), base_env),
	)
	.await;

	// managed envs apply on top of the base env; base entries are preserved.
	std::fs::write(&project_file, r#"{"envs": {"MY_KEY": "value"}}"#).unwrap();
	let _ = env.cmd(false).args(["start", "alpha"]).output().unwrap();
	assert_eq!(
		persisted_env(&env, "alpha").await,
		vec![
			"PATH=/home/agent/.cargo/bin:/usr/bin".to_string(),
			"LANG=en_US.UTF-8".to_string(),
			"TERM=xterm-256color".to_string(),
			format!("SVAMM_REGISTRY_PROXY_PORT={}", env.proxy_port),
			"MY_KEY=value".to_string(),
		],
		"base env entries must survive reconciliation"
	);

	// removing the `envs` key entirely drops only the svamm-managed key;
	// base/image entries stay.
	std::fs::write(&project_file, "{}").unwrap();
	let _ = env.cmd(false).args(["restart", "alpha"]).output().unwrap();
	assert_eq!(
		persisted_env(&env, "alpha").await,
		vec![
			"PATH=/home/agent/.cargo/bin:/usr/bin".to_string(),
			"LANG=en_US.UTF-8".to_string(),
			"TERM=xterm-256color".to_string(),
			format!("SVAMM_REGISTRY_PROXY_PORT={}", env.proxy_port),
		],
		"removed managed keys must drop while foreign entries stay"
	);
}
