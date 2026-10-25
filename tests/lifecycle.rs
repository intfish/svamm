//! lifecycle command tests: list, net, start, stop, restart, rm, run.
//!
//! sandboxes are seeded directly into the msb sqlite database. dry-run mode
//! covers command construction. db-backed reads run for real.

mod common;

use common::{DIGEST_A, DIGEST_B, TestEnv, oci_config};

/// seed a sandbox whose main rw mount points at a host project directory.
async fn seed_project_sandbox(env: &TestEnv, name: &str, status: &str, proj_host: &str) {
	let mut config = oci_config(env.port, name, "/home/agent/proj", DIGEST_A);
	config["mounts"] = serde_json::json!([
		{
			"type": "Bind",
			"host": proj_host,
			"guest": "/home/agent/proj",
			"options": {"readonly": false}
		}
	]);
	env.seed_sandbox(name, status, Some(("svamm", "managed")), config)
		.await;
}

async fn write_project_ports(dir: &std::path::Path, content: &str) {
	std::fs::write(dir.join("svamm.json"), content).unwrap();
}

async fn seed_alpha(env: &TestEnv) {
	env.seed_sandbox(
		"alpha",
		"Running",
		Some(("svamm", "managed")),
		oci_config(env.port, "alpha", "/home/agent/proj", DIGEST_A),
	)
	.await;
}

async fn seed_delta(env: &TestEnv) {
	env.seed_sandbox(
		"delta",
		"Stopped",
		Some(("svamm", "managed")),
		oci_config(env.port, "delta", "/home/agent", DIGEST_B),
	)
	.await;
}

#[tokio::test]
async fn list_filters_by_svamm_label() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	// beta carries no svamm label and must be excluded.
	env.seed_sandbox(
		"beta",
		"Stopped",
		None,
		oci_config(env.port, "beta", "/home/agent", DIGEST_B),
	)
	.await;
	let stdout = env.run_ok(false, &["list"]);
	assert!(stdout.contains("alpha"));
	assert!(!stdout.contains("beta"));
	let quiet = env.run_ok(false, &["list", "-q"]);
	assert_eq!(quiet.trim(), "alpha");
}

#[tokio::test]
async fn list_ls_alias_works() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	let stdout = env.run_ok(false, &["ls", "-q"]);
	assert_eq!(stdout.trim(), "alpha");
}

#[tokio::test]
async fn list_prints_status_table() {
	let env = TestEnv::new();
	env.init_db().await;
	let mut config = oci_config(env.port, "alpha", "/home/agent/proj", DIGEST_A);
	config["resources"] = serde_json::json!({
		"cpus": 4,
		"memory_mib": 8192,
		"max_cpus": 8,
		"max_memory_mib": 16384
	});
	env.seed_sandbox("alpha", "Running", Some(("svamm", "managed")), config)
		.await;
	let stdout = env.run_ok(false, &["list"]);
	assert!(stdout.contains("NAME"), "missing NAME header: {stdout}");
	assert!(stdout.contains("IMAGE"), "missing IMAGE header: {stdout}");
	assert!(stdout.contains("CPUS"), "missing CPUS header: {stdout}");
	assert!(stdout.contains("MEM"), "missing MEM header: {stdout}");
	assert!(stdout.contains("STATUS"), "missing STATUS header: {stdout}");
	assert!(stdout.contains("alpha"), "missing sandbox name: {stdout}");
	assert!(stdout.contains("running"), "missing status: {stdout}");
	assert!(
		stdout.contains(&format!("127.0.0.1:{}/svamm:latest", env.port)),
		"missing image: {stdout}"
	);
	assert!(stdout.contains("4 / 8"), "missing cpus: {stdout}");
	assert!(stdout.contains("8 GiB / 16 GiB"), "missing mem: {stdout}");
}

#[tokio::test]
async fn list_table_hides_duplicate_when_effective_equals_max() {
	// a default sandbox has effective == max. the ls table must show a plain
	// value instead of `4 / 4` / `8 GiB / 8 GiB`.
	let env = TestEnv::new();
	env.init_db().await;
	let mut config = oci_config(env.port, "alpha", "/home/agent/proj", DIGEST_A);
	config["resources"] = serde_json::json!({
		"cpus": 4,
		"memory_mib": 8192,
		"max_cpus": 4,
		"max_memory_mib": 8192
	});
	env.seed_sandbox("alpha", "Running", Some(("svamm", "managed")), config)
		.await;
	let stdout = env.run_ok(false, &["list"]);
	let row = stdout
		.lines()
		.find(|line| line.contains("alpha"))
		.expect("alpha row");
	assert!(
		row.contains("4") && !row.contains("4 / 4"),
		"non-burstable cpus must be a plain value: {stdout}"
	);
	assert!(
		row.contains("8 GiB") && !row.contains("8 GiB / 8 GiB"),
		"non-burstable mem must be a plain value: {stdout}"
	);
}

#[tokio::test]
async fn list_table_shows_stopped_status() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_delta(&env).await;
	let stdout = env.run_ok(false, &["list"]);
	assert!(stdout.contains("delta"), "missing sandbox name: {stdout}");
	assert!(
		stdout.contains("stopped"),
		"missing stopped status: {stdout}"
	);
}

#[tokio::test]
async fn list_empty_reports_note() {
	let env = TestEnv::new();
	env.init_db().await;
	let stdout = env.run_ok(false, &["list"]);
	assert!(
		stdout.contains("no svamm sandboxes"),
		"expected empty note; got: {stdout}"
	);
}

#[tokio::test]
async fn net_prints_sandbox_network_policy() {
	let env = TestEnv::new();
	env.init_db().await;
	let mut config = oci_config(env.port, "alpha", "/home/agent/proj", DIGEST_A);
	config["network"] = serde_json::json!({
		"enabled": true,
		"policy": {
			"default_egress": "allow",
			"default_ingress": "deny",
			"rules": [
				{"direction": "egress", "destination": {"group": "host"}, "action": "allow", "protocols": ["tcp"], "ports": [{"start": env.port, "end": env.port}]},
				{"direction": "egress", "destination": {"domain": "example.com"}, "action": "allow", "protocols": ["tcp"], "ports": [{"start": 443, "end": 443}]},
				{"direction": "egress", "destination": {"domain_suffix": "tracking.com"}, "action": "deny", "protocols": [], "ports": []}
			]
		},
		"ports": [
			{"host_port": 9000, "guest_port": 80, "protocol": "tcp", "host_bind": "127.0.0.1"},
			{"host_port": 8081, "guest_port": 81, "protocol": "udp", "host_bind": "0.0.0.0"}
		]
	});
	env.seed_sandbox("alpha", "Stopped", Some(("svamm", "managed")), config)
		.await;
	let stdout = env.run_ok(false, &["net", "alpha"]);
	assert!(stdout.contains("default egress:  allow"));
	assert!(stdout.contains("default ingress: deny"));
	assert!(stdout.contains("ACTION"));
	assert!(stdout.contains("DIRECTION"));
	assert!(stdout.contains("DEST"));
	assert!(stdout.contains("PROTOCOL"));
	assert!(stdout.contains("PORT"));
	let port = env.port.to_string();
	let rows: [[&str; 5]; 3] = [
		["allow", "egress", "host", "tcp", &port],
		["allow", "egress", "example.com", "tcp", "443"],
		["deny", "egress", "*.tracking.com", "any", "any"],
	];
	for cells in rows {
		// each rule renders as one aligned table row.
		assert!(
			stdout
				.lines()
				.any(|line| line.split_whitespace().eq(cells.iter().copied())),
			"missing rule row {cells:?}; got: {stdout}"
		);
	}
	assert!(stdout.contains("published ports (2):"));
	assert!(stdout.contains("HOST"));
	assert!(stdout.contains("GUEST"));
	assert!(
		stdout.lines().any(|line| line
			.split_whitespace()
			.eq(["9000", "80", "tcp"].iter().copied())),
		"missing tcp port row; got: {stdout}"
	);
	assert!(
		stdout.lines().any(|line| line
			.split_whitespace()
			.eq(["0.0.0.0:8081", "81", "udp"].iter().copied())),
		"missing udp port row; got: {stdout}"
	);
}

#[tokio::test]
async fn net_reports_sandbox_without_policy() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_delta(&env).await;
	let stdout = env.run_ok(false, &["net", "delta"]);
	assert!(stdout.contains("has no network policy"));
}

#[tokio::test]
async fn net_prints_published_ports_without_policy() {
	let env = TestEnv::new();
	env.init_db().await;
	let mut config = oci_config(env.port, "alpha", "/home/agent/proj", DIGEST_A);
	config["network"] = serde_json::json!({
		"enabled": true,
		"ports": [
			{"host_port": 9000, "guest_port": 80, "protocol": "tcp", "host_bind": "127.0.0.1"},
			{"host_port": 5353, "guest_port": 53, "protocol": "udp", "host_bind": "127.0.0.1"}
		]
	});
	env.seed_sandbox("alpha", "Stopped", Some(("svamm", "managed")), config)
		.await;
	let stdout = env.run_ok(false, &["net", "alpha"]);
	assert!(stdout.contains("published ports (2):"));
	assert!(
		stdout.lines().any(|line| line
			.split_whitespace()
			.eq(["9000", "80", "tcp"].iter().copied())),
		"missing tcp port row; got: {stdout}"
	);
	assert!(
		stdout.lines().any(|line| line
			.split_whitespace()
			.eq(["5353", "53", "udp"].iter().copied())),
		"missing udp port row; got: {stdout}"
	);
	assert!(
		!stdout.contains("has no network policy"),
		"ports-only sandbox must not warn about a missing policy"
	);
}

#[tokio::test]
async fn net_fails_for_unknown_sandbox() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(&env, &["net", "nope"], "sandbox 'nope' does not exist");
}

#[tokio::test]
async fn net_requires_a_name() {
	let env = TestEnv::new();
	env.init_db().await;
	env.run_fail(false, &["net"]);
}

#[tokio::test]
async fn net_validates_slug() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(&env, &["net", "bad name"], "invalid sandbox name");
}

#[tokio::test]
async fn mounts_prints_sandbox_mounts() {
	let env = TestEnv::new();
	env.init_db().await;
	let mut config = oci_config(env.port, "alpha", "/home/agent/proj", DIGEST_A);
	config["mounts"] = serde_json::json!([
		{
			"type": "Bind",
			"host": "/home/user/proj",
			"guest": "/home/agent/proj",
			"readonly": false,
			"quota_mib": 32768
		},
		{
			"type": "Bind",
			"host": "/home/user/readonly",
			"guest": "/home/agent/extra",
			"readonly": true
		},
		{
			"type": "Tmpfs",
			"guest": "/tmp",
			"size_mib": 2048,
			"readonly": false
		}
	]);
	env.seed_sandbox("alpha", "Stopped", Some(("svamm", "managed")), config)
		.await;
	let stdout = env.run_ok(false, &["mounts", "alpha"]);
	assert!(
		stdout.contains("mounts (3):"),
		"missing mounts header; got: {stdout}"
	);
	assert!(
		stdout.contains("SOURCE") && stdout.contains("DEST"),
		"missing table header; got: {stdout}"
	);
	let rows: Vec<Vec<&str>> = stdout
		.lines()
		.map(|line| line.split_whitespace().collect())
		.collect();
	assert!(
		rows.iter()
			.any(|row| *row == ["/home/user/proj", "/home/agent/proj", "rw", "32", "GiB"]),
		"missing rw mount row; got: {stdout}"
	);
	assert!(
		rows.iter()
			.any(|row| *row == ["/home/user/readonly", "/home/agent/extra", "ro", "-"]),
		"missing ro mount row; got: {stdout}"
	);
	assert!(
		rows.iter()
			.any(|row| *row == ["[tmpfs]", "/tmp", "rw", "2", "GiB"]),
		"missing tmpfs mount row; got: {stdout}"
	);
}

#[tokio::test]
async fn mounts_reports_sandbox_without_mounts() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_delta(&env).await;
	let stdout = env.run_ok(false, &["mounts", "delta"]);
	assert!(stdout.contains("has no mounts"));
}

#[tokio::test]
async fn mounts_fails_for_unknown_sandbox() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(&env, &["mounts", "nope"], "sandbox 'nope' does not exist");
}

#[tokio::test]
async fn mounts_requires_a_name() {
	let env = TestEnv::new();
	env.init_db().await;
	env.run_fail(false, &["mounts"]);
}

#[tokio::test]
async fn mounts_validates_slug() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(&env, &["mounts", "bad name"], "invalid sandbox name");
}

#[tokio::test]
async fn start_passes_through() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	let stdout = env.run_ok(true, &["start", "alpha"]);
	assert_eq!(stdout.trim(), "start alpha");
}

#[tokio::test]
async fn start_fails_for_unknown_sandbox() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(&env, &["start", "nope"], "sandbox 'nope' does not exist");
}

#[tokio::test]
async fn restart_fails_for_unknown_sandbox() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(&env, &["restart", "nope"], "sandbox 'nope' does not exist");
}

#[tokio::test]
async fn stop_passes_through() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	let stdout = env.run_ok(true, &["stop", "alpha"]);
	assert_eq!(stdout.trim(), "stop alpha");
}

#[tokio::test]
async fn stop_all_stops_every_svamm_sandbox() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	seed_delta(&env).await;
	let stdout = env.run_ok(true, &["stop", "-a"]);
	assert!(stdout.contains("stop alpha"));
	assert!(stdout.contains("stop delta"));
}

#[tokio::test]
async fn stop_all_rejects_combined_sandbox_name() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(
		&env,
		&["stop", "-a", "alpha"],
		"-a cannot be combined with a sandbox name",
	);
}

#[tokio::test]
async fn stop_without_name_or_all_fails() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(&env, &["stop"], "missing required argument: name");
}

#[tokio::test]
async fn restart_passes_through() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	let stdout = env.run_ok(true, &["restart", "alpha"]);
	assert_eq!(stdout.trim(), "restart alpha");
}

#[tokio::test]
async fn rm_on_stopped_sandbox_only_removes() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_delta(&env).await;
	let stdout = env.run_ok(true, &["rm", "delta"]);
	assert_eq!(stdout.trim(), "rm delta");
}

#[tokio::test]
async fn rm_on_running_sandbox_stops_then_removes() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	let stdout = env.run_ok(true, &["rm", "alpha"]);
	assert_eq!(stdout.trim(), "stop alpha\nrm alpha");
}

#[tokio::test]
async fn run_requires_existing_sandbox() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(&env, &["run", "nope"], "sandbox 'nope' does not exist");
}

#[tokio::test]
async fn run_opens_opencode_in_sandbox_workdir() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	let stdout = env.run_ok(true, &["run", "alpha"]);
	assert_eq!(
		stdout.trim(),
		"exec -t -w /home/agent/proj alpha -- opencode"
	);
}

#[tokio::test]
async fn run_tty_opens_default_shell() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	let stdout = env.run_ok(true, &["run", "alpha", "-t"]);
	assert_eq!(stdout.trim(), "exec -t alpha -- zsh");
}

#[tokio::test]
async fn run_root_opens_root_shell() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	let stdout = env.run_ok(true, &["run", "alpha", "-r"]);
	assert_eq!(stdout.trim(), "exec -t --user root alpha -- zsh");
}

#[tokio::test]
async fn run_tty_root_opens_root_shell() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	let stdout = env.run_ok(true, &["run", "alpha", "-t", "-r"]);
	assert_eq!(stdout.trim(), "exec -t --user root alpha -- zsh");
}

#[tokio::test]
async fn run_with_command_attaches() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	let stdout = env.run_ok(true, &["run", "alpha", "pwd"]);
	assert_eq!(stdout.trim(), "exec -t alpha -- pwd");
}

#[tokio::test]
async fn run_root_with_command_runs_as_root() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	let stdout = env.run_ok(true, &["run", "alpha", "-r", "id"]);
	assert_eq!(stdout.trim(), "exec -t --user root alpha -- id");
}

#[tokio::test]
async fn run_root_with_dashdash_passthrough_keeps_flags() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	let stdout = env.run_ok(true, &["run", "alpha", "-r", "--", "id", "-un"]);
	assert_eq!(stdout.trim(), "exec -t --user root alpha -- id -un");
}

#[tokio::test]
async fn run_dashdash_passthrough_keeps_flags() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	let stdout = env.run_ok(true, &["run", "alpha", "--", "ls", "-la"]);
	assert_eq!(stdout.trim(), "exec -t alpha -- ls -la");
}

#[tokio::test]
async fn run_with_unknown_option_fails() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_alpha(&env).await;
	common::expect_stderr_contains(&env, &["run", "alpha", "-x"], "unexpected argument '-x'");
}

#[tokio::test]
async fn run_on_stopped_sandbox_starts_first() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_delta(&env).await;
	let stdout = env.run_ok(true, &["run", "delta"]);
	let lines: Vec<&str> = stdout.lines().collect();
	assert_eq!(lines[0], "start delta");
	assert_eq!(lines[1], "exec -t -w /home/agent delta -- opencode");
}

#[tokio::test]
async fn start_materializes_missing_flat_disk_vmdk() {
	// non-dry start pulls and materializes the missing vmdk from the fake oci
	// registry before the boot attempt fails.
	let mut env = TestEnv::new();
	env.init_db().await;
	env.seed_registry().await;
	let fake = common::start_fake_oci_registry();
	env.port = fake.port;
	seed_delta(&env).await;

	let vmdk_dir = env.msb_home().join("cache/vmdk");
	std::fs::create_dir_all(&vmdk_dir).unwrap();
	assert_eq!(std::fs::read_dir(&vmdk_dir).unwrap().count(), 0);

	let _ = env.run_fail(false, &["start", "delta"]);

	let files: Vec<String> = std::fs::read_dir(&vmdk_dir)
		.unwrap()
		.map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
		.collect();
	assert!(
		files.iter().any(|name| name.ends_with(".vmdk")),
		"expected a materialized vmdk, found: {files:?}"
	);
}

#[tokio::test]
async fn start_does_not_pull_when_vmdk_present() {
	let env = TestEnv::new();
	env.init_db().await;
	seed_delta(&env).await;
	env.seed_vmdk_file(DIGEST_B).await;
	// dry-run: ensure_startable skips pulls entirely.
	let stdout = env.run_ok(true, &["start", "delta"]);
	assert_eq!(stdout.trim(), "start delta");
}

#[tokio::test]
async fn start_brings_up_stopped_registry_sandbox() {
	// non-dry start must bring the registry back up when its sandbox exists
	// but is stopped. the registry step must always be emitted.
	let env = TestEnv::new();
	env.init_db().await;
	env.seed_sandbox(
		"svamm-registry",
		"Stopped",
		Some(("svamm", "registry")),
		serde_json::json!({"name": "svamm-registry", "labels": {"svamm": "registry"}}),
	)
	.await;
	seed_delta(&env).await;
	env.seed_vmdk_file(DIGEST_B).await;

	let output = env.cmd(false).args(["start", "delta"]).output().unwrap();
	let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
	assert!(
		stdout.contains("starting local image registry sandbox"),
		"start must attempt to bring up the stopped registry; stdout:\n{stdout}"
	);
}

#[tokio::test]
async fn restart_applies_project_ports_dry_run() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	write_project_ports(proj.path(), r#"{"network": {"ports": ["9000:80"]}}"#).await;
	seed_project_sandbox(&env, "alpha", "Stopped", proj.path().to_str().unwrap()).await;
	let stdout = env.run_ok(true, &["restart", "alpha"]);
	assert!(
		stdout.contains("apply ports alpha: 9000:80"),
		"unexpected stdout: {stdout}"
	);
	assert!(
		stdout.contains("restart alpha"),
		"unexpected stdout: {stdout}"
	);
}

#[tokio::test]
async fn start_applies_project_ports_dry_run() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	write_project_ports(proj.path(), r#"{"network": {"ports": ["9000:80"]}}"#).await;
	seed_project_sandbox(&env, "alpha", "Stopped", proj.path().to_str().unwrap()).await;
	let stdout = env.run_ok(true, &["start", "alpha"]);
	assert!(
		stdout.contains("apply ports alpha: 9000:80"),
		"unexpected stdout: {stdout}"
	);
	assert!(
		stdout.contains("start alpha"),
		"unexpected stdout: {stdout}"
	);
}

#[tokio::test]
async fn run_applies_project_ports_on_stopped_dry_run() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	write_project_ports(proj.path(), r#"{"network": {"ports": ["9000:80"]}}"#).await;
	seed_project_sandbox(&env, "alpha", "Stopped", proj.path().to_str().unwrap()).await;
	let stdout = env.run_ok(true, &["run", "alpha"]);
	assert!(
		stdout.contains("apply ports alpha: 9000:80"),
		"unexpected stdout: {stdout}"
	);
	assert!(
		stdout.contains("apply net-rules alpha:"),
		"unexpected stdout: {stdout}"
	);
	assert!(
		stdout.contains("start alpha"),
		"unexpected stdout: {stdout}"
	);
}

#[tokio::test]
async fn restart_skips_when_ports_unchanged() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	write_project_ports(proj.path(), r#"{"network": {"ports": ["9000:80"]}}"#).await;
	let mut config = oci_config(env.port, "alpha", "/home/agent/proj", DIGEST_A);
	config["mounts"] = serde_json::json!([
		{
			"type": "Bind",
			"host": proj.path().to_str().unwrap(),
			"guest": "/home/agent/proj",
			"options": {"readonly": false}
		}
	]);
	config["network"] = serde_json::json!({
		"enabled": true,
		"policy": env
			.create_config(&["alpha", proj.path().to_str().unwrap()])["network"]["policy"]
			.clone(),
		"ports": [{"host_port": 9000, "guest_port": 80, "protocol": "tcp", "host_bind": "127.0.0.1"}]
	});
	env.seed_sandbox("alpha", "Stopped", Some(("svamm", "managed")), config)
		.await;
	let stdout = env.run_ok(true, &["restart", "alpha"]);
	assert_eq!(stdout.trim(), "restart alpha");
}

#[tokio::test]
async fn restart_empty_ports_clears_existing() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	write_project_ports(proj.path(), r#"{"network": {"ports": []}}"#).await;
	let mut config = oci_config(env.port, "alpha", "/home/agent/proj", DIGEST_A);
	config["mounts"] = serde_json::json!([
		{
			"type": "Bind",
			"host": proj.path().to_str().unwrap(),
			"guest": "/home/agent/proj",
			"options": {"readonly": false}
		}
	]);
	config["network"] = serde_json::json!({
		"enabled": true,
		"ports": [{"host_port": 9000, "guest_port": 80, "protocol": "tcp", "host_bind": "127.0.0.1"}]
	});
	env.seed_sandbox("alpha", "Stopped", Some(("svamm", "managed")), config)
		.await;
	let stdout = env.run_ok(true, &["restart", "alpha"]);
	assert!(
		stdout.contains("apply ports alpha: none"),
		"unexpected stdout: {stdout}"
	);
}

#[tokio::test]
async fn restart_updates_persisted_ports_without_recreate() {
	// non-dry restart on a stopped sandbox must update the durable msb config
	// before the boot attempt, so changed ports apply without recreating the vm.
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	write_project_ports(proj.path(), r#"{"network": {"ports": ["9000:80"]}}"#).await;
	seed_project_sandbox(&env, "alpha", "Stopped", proj.path().to_str().unwrap()).await;
	env.seed_registry().await;
	env.seed_vmdk_file(DIGEST_A).await;

	let _ = env.cmd(false).args(["restart", "alpha"]).output().unwrap();

	// the sdk must still read the updated row.
	let _ = env.run_ok(false, &["net", "alpha"]);

	let db = env.db().await;
	let stored: String = sqlx::query_scalar("SELECT config FROM sandbox WHERE name = 'alpha'")
		.fetch_one(&db)
		.await
		.unwrap();
	let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
	let ports = stored["network"]["ports"].as_array().unwrap();
	assert_eq!(ports.len(), 1, "persisted config: {stored}");
	assert_eq!(ports[0]["host_port"], 9000);
	assert_eq!(ports[0]["guest_port"], 80);
	assert_eq!(ports[0]["protocol"], "tcp");
}

async fn write_project_net_rules(dir: &std::path::Path, content: &str) {
	std::fs::write(dir.join("svamm.json"), content).unwrap();
}

#[tokio::test]
async fn restart_applies_project_net_rules_dry_run() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	write_project_net_rules(
		proj.path(),
		r#"{"network": {"allow": ["svamm-project.test:tcp:443"]}}"#,
	)
	.await;
	seed_project_sandbox(&env, "alpha", "Stopped", proj.path().to_str().unwrap()).await;
	let stdout = env.run_ok(true, &["restart", "alpha"]);
	assert!(
		stdout.contains("apply net-rules alpha:"),
		"unexpected stdout: {stdout}"
	);
	assert!(
		stdout.contains("allow@svamm-project.test:tcp:443"),
		"unexpected stdout: {stdout}"
	);
	assert!(
		stdout.contains("restart alpha"),
		"unexpected stdout: {stdout}"
	);
}

#[tokio::test]
async fn start_applies_project_net_rules_dry_run() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	write_project_net_rules(
		proj.path(),
		r#"{"network": {"allow": ["svamm-project.test:tcp:443"]}}"#,
	)
	.await;
	seed_project_sandbox(&env, "alpha", "Stopped", proj.path().to_str().unwrap()).await;
	let stdout = env.run_ok(true, &["start", "alpha"]);
	assert!(
		stdout.contains("apply net-rules alpha:"),
		"unexpected stdout: {stdout}"
	);
	assert!(
		stdout.contains("allow@svamm-project.test:tcp:443"),
		"unexpected stdout: {stdout}"
	);
	assert!(
		stdout.contains("start alpha"),
		"unexpected stdout: {stdout}"
	);
}

#[tokio::test]
async fn restart_skips_when_net_rules_unchanged() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	write_project_net_rules(
		proj.path(),
		r#"{"network": {"allow": ["svamm-project.test:tcp:443"]}}"#,
	)
	.await;
	// seed the exact policy this project produces. the reconcile is then a no-op.
	let mut config = env.create_config(&["alpha", proj.path().to_str().unwrap()]);
	config["mounts"] = serde_json::json!([
		{
			"type": "Bind",
			"host": proj.path().to_str().unwrap(),
			"guest": "/home/agent/proj",
			"options": {"readonly": false}
		}
	]);
	env.seed_sandbox("alpha", "Stopped", Some(("svamm", "managed")), config)
		.await;
	let stdout = env.run_ok(true, &["restart", "alpha"]);
	assert_eq!(stdout.trim(), "restart alpha");
}

#[tokio::test]
async fn restart_drops_cleared_project_net_rule_dry_run() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	write_project_net_rules(
		proj.path(),
		r#"{"network": {"allow": ["svamm-project.test:tcp:443"]}}"#,
	)
	.await;
	seed_project_sandbox(&env, "alpha", "Stopped", proj.path().to_str().unwrap()).await;
	let _ = env.run_ok(true, &["restart", "alpha"]);

	// remove the project rule. the next restart must fall back to base and registry.
	write_project_net_rules(proj.path(), r#"{"network": {"allow": []}}"#).await;
	let stdout = env.run_ok(true, &["restart", "alpha"]);
	assert!(
		stdout.contains("apply net-rules alpha:"),
		"unexpected stdout: {stdout}"
	);
	assert!(
		!stdout.contains("allow@svamm-project.test:tcp:443"),
		"cleared project rule still present: {stdout}"
	);
	assert!(
		stdout.contains(&format!("allow@host:tcp:{}", env.proxy_port)),
		"registry rule must be preserved: {stdout}"
	);
}

#[tokio::test]
async fn restart_updates_persisted_net_rules_without_recreate() {
	// non-dry restart on a stopped sandbox must update the durable msb config
	// before the boot attempt, so changed net rules apply without recreating the vm.
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	write_project_net_rules(
		proj.path(),
		r#"{"network": {"allow": ["svamm-project.test:tcp:443"]}}"#,
	)
	.await;
	seed_project_sandbox(&env, "alpha", "Stopped", proj.path().to_str().unwrap()).await;
	env.seed_registry().await;
	env.seed_vmdk_file(DIGEST_A).await;

	let _ = env.cmd(false).args(["restart", "alpha"]).output().unwrap();

	// the sdk must still read the updated row.
	let _ = env.run_ok(false, &["net", "alpha"]);

	let db = env.db().await;
	let stored: String = sqlx::query_scalar("SELECT config FROM sandbox WHERE name = 'alpha'")
		.fetch_one(&db)
		.await
		.unwrap();
	let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
	let policy = &stored["network"]["policy"];
	assert_eq!(
		policy["default_egress"], "deny",
		"persisted config: {stored}"
	);
	assert_eq!(
		policy["default_ingress"], "allow",
		"persisted config: {stored}"
	);
	let rules = policy["rules"].as_array().unwrap();
	let has_project_rule = rules.iter().any(|rule| {
		rule["destination"].get("domain").and_then(|d| d.as_str()) == Some("svamm-project.test")
	});
	assert!(
		has_project_rule,
		"project rule must be persisted into the policy: {stored}"
	);
	let has_registry_rule = rules.iter().any(|rule| {
		rule["direction"] == "egress"
			&& rule["action"] == "allow"
			&& rule["destination"].get("group").and_then(|g| g.as_str()) == Some("host")
			&& rule["ports"].as_array().is_some_and(|ports| {
				ports.iter().any(|range| {
					range["start"] == serde_json::json!(env.proxy_port)
						&& range["end"] == serde_json::json!(env.proxy_port)
				})
			})
	});
	assert!(
		has_registry_rule,
		"registry host rule must be persisted: {stored}"
	);
}

/// seed a stopped sandbox with a 0.6.18-era persisted config (no `network.strict` key),
/// as if created before the msb 0.7.3 strict-default change.
async fn seed_legacy_sandbox(env: &TestEnv) {
	let mut config = oci_config(env.port, "alpha", "/home/agent/proj", DIGEST_A);
	config["network"] = serde_json::json!({
		"enabled": true,
		"policy": {
			"default_egress": "deny",
			"default_ingress": "allow",
			"rules": []
		}
	});
	env.seed_sandbox("alpha", "Stopped", Some(("svamm", "managed")), config)
		.await;
}

async fn stored_config(env: &TestEnv) -> serde_json::Value {
	let db = env.db().await;
	let stored: String = sqlx::query_scalar("SELECT config FROM sandbox WHERE name = 'alpha'")
		.fetch_one(&db)
		.await
		.unwrap();
	serde_json::from_str(&stored).unwrap()
}

#[tokio::test]
async fn start_heals_persisted_strict_hostname_policy_without_recreate() {
	// a sandbox created before the msb 0.7.3 strict-default change must be
	// repaired in place on the next start: the effective strictness (from the
	// default network.yml, which is off) is written into its persisted config.
	let env = TestEnv::new();
	env.init_db().await;
	seed_legacy_sandbox(&env).await;
	env.seed_registry().await;
	env.seed_vmdk_file(DIGEST_A).await;

	// init_db ran `list`, which installs the embedded default network.yml
	// (strict: false) into the hermetic config dir.
	let _ = env.cmd(false).args(["start", "alpha"]).output().unwrap();
	let _ = env.run_ok(false, &["net", "alpha"]);

	let stored = stored_config(&env).await;
	assert_eq!(
		stored["network"]["strict"], false,
		"persisted config: {stored}"
	);
}

#[tokio::test]
async fn start_honors_explicit_strict_true_in_network_config() {
	// an explicit `strict: true` in the user's network.yml wins over the
	// default; a sandbox persisted with `strict: false` is aligned to it on
	// the next start.
	let env = TestEnv::new();
	std::fs::create_dir_all(env.config_dir()).unwrap();
	std::fs::write(env.config_dir().join("network.yml"), "strict: true\n").unwrap();
	env.init_db().await;
	let mut config = oci_config(env.port, "alpha", "/home/agent/proj", DIGEST_A);
	config["network"] = serde_json::json!({
		"enabled": true,
		"strict": false,
		"policy": {"default_egress": "deny", "default_ingress": "allow", "rules": []}
	});
	env.seed_sandbox("alpha", "Stopped", Some(("svamm", "managed")), config)
		.await;
	env.seed_registry().await;
	env.seed_vmdk_file(DIGEST_A).await;

	let _ = env.cmd(false).args(["start", "alpha"]).output().unwrap();
	let _ = env.run_ok(false, &["net", "alpha"]);

	let stored = stored_config(&env).await;
	assert_eq!(
		stored["network"]["strict"], true,
		"persisted config: {stored}"
	);
}

#[tokio::test]
async fn start_skips_strict_reconcile_when_already_effective() {
	// persisted strict already matches the effective value: no change.
	let env = TestEnv::new();
	env.init_db().await;
	let mut config = oci_config(env.port, "alpha", "/home/agent/proj", DIGEST_A);
	config["network"] = serde_json::json!({
		"enabled": true,
		"strict": false,
		"policy": {"default_egress": "deny", "default_ingress": "allow", "rules": []}
	});
	env.seed_sandbox("alpha", "Stopped", Some(("svamm", "managed")), config)
		.await;
	env.seed_registry().await;
	env.seed_vmdk_file(DIGEST_A).await;

	let _ = env.cmd(false).args(["start", "alpha"]).output().unwrap();
	let _ = env.run_ok(false, &["net", "alpha"]);

	let stored = stored_config(&env).await;
	assert_eq!(
		stored["network"]["strict"], false,
		"persisted config: {stored}"
	);
}
