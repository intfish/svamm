//! `svamm start`/`restart` mount reconciliation tests.
//!
//! The per-sandbox replay state (in the svamm state dir) records the
//! create-time mount inputs. Starting a sandbox re-evaluates the project
//! `svamm.json` mounts and rewrites the durable `mounts` array so the change
//! applies on the next boot, without recreating the vm. A sandbox without a
//! state is left untouched.

mod common;

use common::{TestEnv, DIGEST_B};

/// Build the seeded config for a flat sandbox whose main rw mount is `proj`
/// (which must already contain a `svamm.json`) and whose durable mounts start
/// as `mounts`.
fn seed_config(env: &TestEnv, mounts: serde_json::Value) -> serde_json::Value {
	let mut config = serde_json::json!({
		"name": "alpha",
		"image": {"Oci": {
			"reference": format!("127.0.0.1:{}/svamm:latest", env.port),
			"root_disk": {"kind": "flat", "size_mib": 10240, "fstype": "ext4", "clone": "auto"}
		}},
		"manifest_digest": DIGEST_B,
		"runtime": {"workdir": "/home/agent/proj", "shell": "/bin/sh", "scripts": {}},
		"labels": {"svamm": "managed"},
		"env": [],
		"rlimits": [],
		"pull_policy": "IfMissing",
		"security_profile": "default",
		"deployment_profile": "single_tenant",
		"lifecycle": {"ephemeral": false, "max_duration_secs": null, "idle_timeout_secs": null},
	});
	config["mounts"] = mounts;
	config
}

/// the recorded create-time spec for the main rw mount.
fn main_spec(proj: &std::path::Path) -> String {
	format!("{}:/home/agent/proj:rw,quota=32G", proj.display())
}

/// the guests a base create claims as svamm-owned.
fn base_applied() -> Vec<String> {
	vec![
		"/tmp".to_string(),
		"/home/agent/proj".to_string(),
		"/home/agent/proj/svamm.json".to_string(),
	]
}

/// seed a sandbox whose create recorded the standard main mount replay inputs.
async fn seed_replayed(env: &TestEnv, proj: &std::path::Path, mounts: serde_json::Value) {
	seed_replayed_applied(env, proj, mounts, base_applied().as_slice()).await;
}

/// seed a sandbox with explicit replay inputs.
async fn seed_replayed_applied(
	env: &TestEnv,
	proj: &std::path::Path,
	mounts: serde_json::Value,
	applied: &[String],
) {
	env.seed_sandbox(
		"alpha",
		"Stopped",
		Some(("svamm", "managed")),
		seed_config(env, mounts),
	)
	.await;
	env.seed_mount_state(
		"alpha",
		std::slice::from_ref(&main_spec(proj)),
		"/tmp",
		"2G",
		applied,
	);
}

/// the persisted shape for the built-in mounts of the seeded sandbox.
fn base_mounts(env: &TestEnv, proj: &std::path::Path) -> serde_json::Value {
	serde_json::json!([
		{
			"type": "Tmpfs", "guest": "/tmp", "size_mib": 2048,
			"options": {"readonly": false, "noexec": false, "nosuid": false, "nodev": false}
		},
		{
			"type": "Bind", "host": proj.display().to_string(), "guest": "/home/agent/proj",
			"options": {"readonly": false, "noexec": false, "nosuid": false, "nodev": false},
			"stat_virtualization": "strict", "host_permissions": "private",
			"follow_root_symlinks": false, "quota_mib": 32768
		},
		{
			"type": "Bind", "host": env.mask_file().display().to_string(), "guest": "/home/agent/proj/svamm.json",
			"options": {"readonly": true, "noexec": false, "nosuid": false, "nodev": false},
			"stat_virtualization": "strict", "host_permissions": "private",
			"follow_root_symlinks": false, "quota_mib": null
		},
	])
}

/// fetch the persisted config for `name` from the seeded database.
async fn persisted_config(env: &TestEnv, name: &str) -> serde_json::Value {
	let db = env.db().await;
	let config: String =
		sqlx::query_scalar::<_, String>("SELECT config FROM sandbox WHERE name = ?")
			.bind(name)
			.fetch_one(&db)
			.await
			.unwrap();
	serde_json::from_str(&config).unwrap()
}

/// fetch the persisted mounts array for `name`.
async fn persisted_mounts(env: &TestEnv, name: &str) -> Vec<serde_json::Value> {
	persisted_config(env, name)
		.await
		.get("mounts")
		.and_then(serde_json::Value::as_array)
		.cloned()
		.unwrap_or_default()
}

#[tokio::test]
async fn start_reconciles_added_project_mount() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	let extra = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		format!(
			r#"{{"mounts": [{{"src": "{}", "dest": "/home/agent/extra", "mode": "ro"}}]}}"#,
			extra.path().display()
		),
	)
	.unwrap();
	seed_replayed(&env, proj.path(), base_mounts(&env, proj.path())).await;

	let stdout = env.run_ok(true, &["start", "alpha"]);
	assert!(
		stdout.contains("apply mounts alpha"),
		"dry-run must preview mount changes; stdout:\n{stdout}"
	);
	assert!(
		stdout.contains(&format!(
			"+ {}:/home/agent/extra:ro",
			extra.path().display()
		)),
		"added mount must be previewed as an addition; stdout:\n{stdout}"
	);
	assert!(
		!stdout.contains("\n  - "),
		"nothing is removed in this change; stdout:\n{stdout}"
	);
}

#[tokio::test]
async fn start_persists_project_mount_change_and_keeps_foreign_mounts() {
	let env = TestEnv::new();
	env.init_db().await;
	env.seed_registry().await;
	env.seed_vmdk_file(DIGEST_B).await;
	let proj = tempfile::tempdir().unwrap();
	let extra = tempfile::tempdir().unwrap();
	let foreign = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		format!(
			r#"{{"mounts": [{{"src": "{}", "dest": "/home/agent/extra", "mode": "rw"}}]}}"#,
			extra.path().display()
		),
	)
	.unwrap();
	let mut mounts = base_mounts(&env, proj.path());
	mounts.as_array_mut().unwrap().push(serde_json::json!({
		"type": "Bind", "host": foreign.path().display().to_string(), "guest": "/srv/foreign",
		"options": {"readonly": true, "noexec": false, "nosuid": false, "nodev": false},
		"stat_virtualization": "strict", "host_permissions": "private",
		"follow_root_symlinks": false, "quota_mib": null
	}));
	seed_replayed(&env, proj.path(), mounts).await;

	// The boot fails (no KVM/msb in this environment); the durable config is
	// what this test verifies.
	let _ = env.cmd(false).args(["start", "alpha"]).output().unwrap();

	let mounts = persisted_mounts(&env, "alpha").await;
	let guests: Vec<String> = mounts
		.iter()
		.map(|m| m["guest"].as_str().unwrap_or("").to_string())
		.collect();
	assert!(
		guests.contains(&"/home/agent/extra".to_string()),
		"new project mount must be persisted: {guests:?}"
	);
	assert!(
		guests.contains(&"/srv/foreign".to_string()),
		"non-svamm mounts must be preserved: {guests:?}"
	);
	let extra_entry = mounts
		.iter()
		.find(|m| m["guest"] == "/home/agent/extra")
		.expect("extra mount entry");
	assert_eq!(
		extra_entry["quota_mib"], 32768,
		"rw project mount keeps its quota"
	);
	// The applied guest set now owns the new mount.
	let applied: Vec<String> = env.read_state("alpha").expect("state after start")["mounts"]
		["applied"]
		.as_array()
		.cloned()
		.unwrap_or_default()
		.iter()
		.filter_map(|v| v.as_str().map(str::to_string))
		.collect();
	assert!(
		applied.contains(&"/home/agent/extra".to_string()),
		"applied set must own the new mount: {applied:?}"
	);
	assert!(
		!applied.contains(&"/srv/foreign".to_string()),
		"foreign mount must not be claimed: {applied:?}"
	);
}

#[tokio::test]
async fn start_drops_removed_project_mount() {
	let env = TestEnv::new();
	env.init_db().await;
	env.seed_registry().await;
	env.seed_vmdk_file(DIGEST_B).await;
	let proj = tempfile::tempdir().unwrap();
	let extra = tempfile::tempdir().unwrap();
	// the project no longer declares any mounts; the previously applied extra
	// mount must be dropped on the next start.
	std::fs::write(proj.path().join("svamm.json"), "{}").unwrap();
	let mut mounts = base_mounts(&env, proj.path());
	mounts.as_array_mut().unwrap().push(serde_json::json!({
		"type": "Bind", "host": extra.path().display().to_string(), "guest": "/home/agent/extra",
		"options": {"readonly": false, "noexec": false, "nosuid": false, "nodev": false},
		"stat_virtualization": "strict", "host_permissions": "private",
		"follow_root_symlinks": false, "quota_mib": 32768
	}));
	let mut applied = base_applied();
	applied.push("/home/agent/extra".to_string());
	seed_replayed_applied(&env, proj.path(), mounts, applied.as_slice()).await;

	// the removed mount must be reported explicitly in the start output.
	let start = env.cmd(false).args(["start", "alpha"]).output().unwrap();
	let start = String::from_utf8_lossy(&start.stdout).into_owned();
	assert!(
		start.contains(&format!(
			"- {}:/home/agent/extra:rw",
			extra.path().display()
		)),
		"removed mount must be reported as a removal; stdout:\n{start}"
	);

	let mounts = persisted_mounts(&env, "alpha").await;
	let guests: Vec<String> = mounts
		.iter()
		.map(|m| m["guest"].as_str().unwrap_or("").to_string())
		.collect();
	assert!(
		!guests.contains(&"/home/agent/extra".to_string()),
		"removed project mount must be dropped: {guests:?}"
	);
	assert!(
		guests.contains(&"/home/agent/proj".to_string()),
		"main mount stays: {guests:?}"
	);
	assert!(
		guests.contains(&"/tmp".to_string()),
		"tmpfs stays: {guests:?}"
	);
}

#[tokio::test]
async fn sandbox_without_state_is_left_alone() {
	// A sandbox without recorded replay inputs cannot be re-evaluated.
	// Starting it must never rewrite its mounts or invent a state.
	let env = TestEnv::new();
	env.init_db().await;
	env.seed_registry().await;
	env.seed_vmdk_file(DIGEST_B).await;
	let proj = tempfile::tempdir().unwrap();
	let extra = tempfile::tempdir().unwrap();
	std::fs::write(proj.path().join("svamm.json"), "{}").unwrap();
	env.seed_sandbox(
		"alpha",
		"Stopped",
		Some(("svamm", "managed")),
		seed_config(&env, base_mounts(&env, proj.path())),
	)
	.await;

	let stdout = env.run_ok(true, &["start", "alpha"]);
	assert!(
		!stdout.contains("apply mounts"),
		"no state: mounts must not be previewed; stdout:\n{stdout}"
	);
	let _ = env.cmd(false).args(["start", "alpha"]).output().unwrap();
	assert!(
		env.read_state("alpha").is_none(),
		"a real start must not record a state for a sandbox without one"
	);

	// even a project mount change never reaches a sandbox without a state.
	std::fs::write(
		proj.path().join("svamm.json"),
		format!(
			r#"{{"mounts": [{{"src": "{}", "dest": "/home/agent/extra", "mode": "ro"}}]}}"#,
			extra.path().display()
		),
	)
	.unwrap();
	let _ = env.cmd(false).args(["restart", "alpha"]).output().unwrap();
	let guests: Vec<String> = persisted_mounts(&env, "alpha")
		.await
		.iter()
		.map(|m| m["guest"].as_str().unwrap_or("").to_string())
		.collect();
	assert!(
		!guests.contains(&"/home/agent/extra".to_string()),
		"a sandbox without a state never re-evaluates: {guests:?}"
	);
}

#[tokio::test]
async fn start_fails_loudly_on_missing_project_mount_source() {
	let env = TestEnv::new();
	env.init_db().await;
	env.seed_registry().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"mounts": [{"src": "/definitely/not/here", "dest": "/home/agent/gone", "mode": "ro"}]}"#,
	)
	.unwrap();
	seed_replayed(&env, proj.path(), base_mounts(&env, proj.path())).await;

	let (stderr, _) = env.run_fail(true, &["start", "alpha"]);
	assert!(
		stderr.contains("mount source does not exist: /definitely/not/here"),
		"expected a loud validation error; stderr:\n{stderr}"
	);
}

/// the persisted mounts of a sandbox created before the mask existed: the
/// overlay binds the real `svamm.json`.
fn legacy_overlay_mounts(proj: &std::path::Path) -> serde_json::Value {
	serde_json::json!([
		{
			"type": "Tmpfs", "guest": "/tmp", "size_mib": 2048,
			"options": {"readonly": false, "noexec": false, "nosuid": false, "nodev": false}
		},
		{
			"type": "Bind", "host": proj.display().to_string(), "guest": "/home/agent/proj",
			"options": {"readonly": false, "noexec": false, "nosuid": false, "nodev": false},
			"stat_virtualization": "strict", "host_permissions": "private",
			"follow_root_symlinks": false, "quota_mib": 32768
		},
		{
			"type": "Bind", "host": format!("{}/svamm.json", proj.display()), "guest": "/home/agent/proj/svamm.json",
			"options": {"readonly": true, "noexec": false, "nosuid": false, "nodev": false},
			"stat_virtualization": "strict", "host_permissions": "private",
			"follow_root_symlinks": false, "quota_mib": null
		},
	])
}

#[tokio::test]
async fn overlay_migrates_to_mask_on_real_start() {
	// A sandbox whose overlay still binds the real svamm.json is re-pointed at
	// the empty mask on its first start, so guests can no longer read the
	// project config. The real file and its content stay untouched.
	let env = TestEnv::new();
	env.init_db().await;
	env.seed_registry().await;
	env.seed_vmdk_file(DIGEST_B).await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"network": {"allow": ["svamm-project.test:tcp:443"]}}"#,
	)
	.unwrap();
	seed_replayed(&env, proj.path(), legacy_overlay_mounts(proj.path())).await;

	let _ = env.cmd(false).args(["start", "alpha"]).output().unwrap();

	let mask_path = env.mask_file();
	assert!(mask_path.is_file(), "mask must exist after a real start");
	assert!(
		std::fs::read(&mask_path).unwrap().is_empty(),
		"mask must stay empty"
	);
	let real = std::fs::read_to_string(proj.path().join("svamm.json")).unwrap();
	assert_eq!(
		real, r#"{"network": {"allow": ["svamm-project.test:tcp:443"]}}"#,
		"the real host config must be untouched"
	);

	let mounts = persisted_mounts(&env, "alpha").await;
	let real_host = format!("{}/svamm.json", proj.path().display());
	assert!(
		mounts.iter().all(|m| m["host"] != real_host),
		"no mount may still bind the real svamm.json: {mounts:?}"
	);
	let overlay = mounts
		.iter()
		.find(|m| m["guest"] == "/home/agent/proj/svamm.json")
		.expect("overlay mount");
	assert_eq!(
		overlay["host"],
		mask_path.display().to_string(),
		"overlay must point at the mask"
	);
	assert_eq!(overlay["options"]["readonly"], true);
}

#[tokio::test]
async fn dry_run_overlay_migration_creates_no_mask() {
	// Dry-run previews the rewrite without touching the host or the database.
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(proj.path().join("svamm.json"), "{}").unwrap();
	seed_replayed(&env, proj.path(), legacy_overlay_mounts(proj.path())).await;

	let stdout = env.run_ok(true, &["start", "alpha"]);
	assert!(
		stdout.contains("apply mounts alpha"),
		"dry-run must preview the rewrite; stdout:\n{stdout}"
	);
	assert!(
		!env.mask_file().exists(),
		"dry-run must not create the mask"
	);
	let mounts = persisted_mounts(&env, "alpha").await;
	let real_host = format!("{}/svamm.json", proj.path().display());
	assert!(
		mounts
			.iter()
			.any(|m| m["guest"] == "/home/agent/proj/svamm.json" && m["host"] == real_host),
		"dry-run must not rewrite the persisted overlay"
	);
}

#[tokio::test]
async fn start_recreates_deleted_mask() {
	// The mask is svamm state: a deleted mask is recreated on the next real
	// start, so the persisted overlay never references a missing file.
	let env = TestEnv::new();
	env.init_db().await;
	env.seed_registry().await;
	env.seed_vmdk_file(DIGEST_B).await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(proj.path().join("svamm.json"), "{}").unwrap();
	seed_replayed(&env, proj.path(), base_mounts(&env, proj.path())).await;

	let _ = env.cmd(false).args(["start", "alpha"]).output().unwrap();
	assert!(env.mask_file().is_file());
	std::fs::remove_file(env.mask_file()).unwrap();
	assert!(!env.mask_file().exists());

	// an unchanged restart still leaves the mask in place for the next boot.
	let _ = env.cmd(false).args(["restart", "alpha"]).output().unwrap();
	assert!(
		env.mask_file().is_file(),
		"a real start must recreate a deleted mask"
	);
}

#[tokio::test]
async fn svamm_json_mode_switches_the_overlay_on_next_start() {
	// The mode is derived from the project file on every start, so switching
	// `svamm_json` between masked and readonly re-points the overlay on the
	// next start, in both directions, without recreating the vm.
	let env = TestEnv::new();
	env.init_db().await;
	env.seed_registry().await;
	env.seed_vmdk_file(DIGEST_B).await;
	let proj = tempfile::tempdir().unwrap();
	let project_config = proj.path().join("svamm.json");
	std::fs::write(&project_config, r#"{"svamm_json": "readonly"}"#).unwrap();
	seed_replayed(&env, proj.path(), base_mounts(&env, proj.path())).await;

	let _ = env.cmd(false).args(["start", "alpha"]).output().unwrap();
	assert!(
		!env.mask_file().exists(),
		"readonly mode must not need the mask"
	);
	let mounts = persisted_mounts(&env, "alpha").await;
	let overlay = mounts
		.iter()
		.find(|m| m["guest"] == "/home/agent/proj/svamm.json")
		.expect("overlay mount");
	let real_host = format!("{}/svamm.json", proj.path().display());
	assert_eq!(
		overlay["host"], real_host,
		"readonly mode must bind the real config"
	);
	assert_eq!(overlay["options"]["readonly"], true);

	// switching back to masked returns the overlay to the empty facade.
	std::fs::write(&project_config, r#"{"svamm_json": "masked"}"#).unwrap();
	let _ = env.cmd(false).args(["restart", "alpha"]).output().unwrap();
	let mounts = persisted_mounts(&env, "alpha").await;
	let overlay = mounts
		.iter()
		.find(|m| m["guest"] == "/home/agent/proj/svamm.json")
		.expect("overlay mount");
	assert_eq!(
		overlay["host"],
		env.mask_file().display().to_string(),
		"masked mode must bind the empty facade"
	);
	assert!(
		env.mask_file().is_file(),
		"masked mode must prepare the mask"
	);
}

#[tokio::test]
async fn start_fails_loudly_on_invalid_svamm_json_mode() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"svamm_json": "hidden"}"#,
	)
	.unwrap();
	seed_replayed(&env, proj.path(), base_mounts(&env, proj.path())).await;
	let (stderr, _) = env.run_fail(true, &["start", "alpha"]);
	assert!(
		stderr.contains("\"svamm_json\" must be \"masked\" or \"readonly\""),
		"expected a loud mode validation error; stderr:\n{stderr}"
	);
}
