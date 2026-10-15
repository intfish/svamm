//! `svamm create` command construction tests.
//!
//! these run the binary in dry-run mode and assert on the resulting `SandboxConfig` JSON

mod common;

use common::TestEnv;

/// find a mount entry by guest path in a serialized sandbox config.
fn mount_by_guest<'a>(config: &'a serde_json::Value, guest: &str) -> Option<&'a serde_json::Value> {
	config
		.get("mounts")?
		.as_array()?
		.iter()
		.find(|m| m.get("guest").and_then(|g| g.as_str()) == Some(guest))
}

fn mounts(config: &serde_json::Value) -> Vec<serde_json::Value> {
	config
		.get("mounts")
		.and_then(|m| m.as_array())
		.cloned()
		.unwrap_or_default()
}

#[tokio::test]
async fn create_full_command_construction() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	let extra = tempfile::tempdir().unwrap();
	let docs = tempfile::tempdir().unwrap();

	let config = env.create_config(&[
		"alpha",
		proj.path().to_str().unwrap(),
		"--rw",
		extra.path().to_str().unwrap(),
		"--ro",
		docs.path().to_str().unwrap(),
	]);

	assert_eq!(config["name"], "alpha");
	assert_eq!(config["labels"]["svamm"], "managed");
	assert_eq!(
		config["image"]["Oci"]["reference"],
		"ghcr.io/intfish/svamm-images/svamm:latest"
	);
	assert_eq!(config["image"]["Oci"]["root_disk"]["kind"], "flat");
	assert_eq!(
		config["runtime"]["workdir"],
		format!(
			"/home/agent/{}",
			proj.path().file_name().unwrap().to_string_lossy()
		)
	);

	// built-in tmpfs at /tmp with 2G.
	let tmpfs = mount_by_guest(&config, "/tmp").expect("tmpfs mount");
	assert_eq!(tmpfs["type"], "Tmpfs");
	assert_eq!(tmpfs["size_mib"], 2048);

	// user mounts land under /home/agent with their modes.
	let extra_mount = mount_by_guest(
		&config,
		&format!(
			"/home/agent/{}",
			extra.path().file_name().unwrap().to_string_lossy()
		),
	)
	.expect("rw mount");
	assert_eq!(extra_mount["options"]["readonly"], false);
	let docs_mount = mount_by_guest(
		&config,
		&format!(
			"/home/agent/{}",
			docs.path().file_name().unwrap().to_string_lossy()
		),
	)
	.expect("ro mount");
	assert_eq!(docs_mount["options"]["readonly"], true);

	// rw workspace mounts carry the configured guest-write quota. ro mounts
	// cannot be grown by the guest, so they do not.
	assert_eq!(extra_mount["quota_mib"], 32768);
	assert!(docs_mount["quota_mib"].is_null());
}

#[tokio::test]
async fn create_mounts_project_svamm_json_read_only() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"network": {"allow": ["svamm-project.test:tcp:443"]}}"#,
	)
	.unwrap();
	let config = env.create_config(&["alpha", proj.path().to_str().unwrap()]);
	let guest = format!(
		"/home/agent/{}/svamm.json",
		proj.path().file_name().unwrap().to_string_lossy()
	);
	let overlay = mount_by_guest(&config, &guest).expect("ro svamm.json overlay");
	assert_eq!(overlay["type"], "Bind");
	assert_eq!(overlay["options"]["readonly"], true);
	assert_eq!(
		overlay["host"],
		env.mask_file().display().to_string(),
		"overlay must mask the real svamm.json with the empty file"
	);
	assert!(
		env.mask_file().is_file(),
		"create must prepare the mask file"
	);
	assert!(
		std::fs::read(env.mask_file()).unwrap().is_empty(),
		"mask must stay empty"
	);
}

#[tokio::test]
async fn create_readonly_mode_mounts_the_real_svamm_json() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"svamm_json": "readonly", "network": {"allow": ["svamm-project.test:tcp:443"]}}"#,
	)
	.unwrap();
	let config = env.create_config(&["alpha", proj.path().to_str().unwrap()]);
	let guest = format!(
		"/home/agent/{}/svamm.json",
		proj.path().file_name().unwrap().to_string_lossy()
	);
	let overlay = mount_by_guest(&config, &guest).expect("ro svamm.json overlay");
	assert_eq!(
		overlay["host"],
		format!("{}/svamm.json", proj.path().display()),
		"readonly mode must bind the real config"
	);
	assert_eq!(overlay["options"]["readonly"], true);
	assert!(
		!env.mask_file().exists(),
		"readonly mode must not create the mask"
	);
}

#[tokio::test]
async fn create_rejects_invalid_svamm_json_mode() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"svamm_json": "hidden"}"#,
	)
	.unwrap();
	let (stderr, _) = env.run_fail(true, &["create", "alpha", proj.path().to_str().unwrap()]);
	assert!(
		stderr.contains("\"svamm_json\" must be \"masked\" or \"readonly\""),
		"expected a loud mode validation error; stderr:\n{stderr}"
	);
}

#[tokio::test]
async fn create_omits_svamm_json_overlay_without_project_file() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	let config = env.create_config(&["alpha", proj.path().to_str().unwrap()]);
	let guest = format!(
		"/home/agent/{}/svamm.json",
		proj.path().file_name().unwrap().to_string_lossy()
	);
	assert!(
		mount_by_guest(&config, &guest).is_none(),
		"no svamm.json overlay without a project file"
	);
}

#[tokio::test]
async fn create_mounts_svamm_json_overlay_on_file_main_mount() {
	let env = TestEnv::new();
	env.init_db().await;
	// a file main mount has no svamm.json overlay.
	let file = tempfile::NamedTempFile::new().unwrap();
	let config = env.create_config(&["alpha", file.path().to_str().unwrap()]);
	let guest = format!(
		"/home/agent/{}/svamm.json",
		file.path().file_name().unwrap().to_string_lossy()
	);
	assert!(mount_by_guest(&config, &guest).is_none());
}

#[tokio::test]
async fn create_quota_override_applies_to_rw_mounts() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	let extra = tempfile::tempdir().unwrap();

	let config = env.create_config(&[
		"beta",
		proj.path().to_str().unwrap(),
		"--rw",
		extra.path().to_str().unwrap(),
		"--quota",
		"64G",
	]);

	let main_mount = mount_by_guest(
		&config,
		&format!(
			"/home/agent/{}",
			proj.path().file_name().unwrap().to_string_lossy()
		),
	)
	.expect("main mount");
	let extra_mount = mount_by_guest(
		&config,
		&format!(
			"/home/agent/{}",
			extra.path().file_name().unwrap().to_string_lossy()
		),
	)
	.expect("rw mount");
	assert_eq!(main_mount["quota_mib"], 64 * 1024);
	assert_eq!(extra_mount["quota_mib"], 64 * 1024);
}

#[tokio::test]
async fn create_rejects_invalid_quota() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();

	common::expect_stderr_contains(
		&env,
		&[
			"create",
			"gamma",
			proj.path().to_str().unwrap(),
			"--quota",
			"1TB",
		],
		"invalid workspace mount quota '1TB' (expected e.g. 32G or 4096M)",
	);
}

#[tokio::test]
async fn create_relative_mounts_are_absolutized() {
	let env = TestEnv::new();
	env.init_db().await;
	let cwd = std::env::current_dir().unwrap();
	let proj = tempfile::tempdir().unwrap();
	std::fs::create_dir(proj.path().join("relative-dir")).unwrap();
	std::env::set_current_dir(proj.path()).unwrap();
	let config = env.create_config(&["alpha", "relative-dir"]);
	std::env::set_current_dir(cwd).unwrap();

	let mount = mount_by_guest(&config, "/home/agent/relative-dir").expect("mount");
	assert_eq!(
		mount["host"],
		proj.path().join("relative-dir").to_str().unwrap()
	);
}

#[tokio::test]
async fn create_single_file_mount_uses_bind() {
	let env = TestEnv::new();
	env.init_db().await;
	let file = tempfile::NamedTempFile::new().unwrap();
	let name = file
		.path()
		.file_name()
		.unwrap()
		.to_string_lossy()
		.into_owned();
	let config = env.create_config(&["alpha", file.path().to_str().unwrap()]);
	let mount = mount_by_guest(&config, &format!("/home/agent/{name}")).expect("file mount");
	assert_eq!(mount["type"], "Bind");
	assert_eq!(mount["host"], file.path().to_str().unwrap());
}

#[tokio::test]
async fn create_missing_mount_source_fails() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(
		&env,
		&["create", "alpha", "/definitely/not/here"],
		"mount source does not exist",
	);
}

#[tokio::test]
async fn create_slug_validation_rejects_spaces() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(&env, &["create", "bad name"], "invalid sandbox name");
}

#[tokio::test]
async fn create_conflict_between_rw_and_ro_mounts() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(
		&env,
		&["create", "alpha", "--rw", "/a/dup", "--ro", "/b/dup"],
		"mount destination conflict: '/home/agent/dup'",
	);
}

#[tokio::test]
async fn create_conflict_with_tmpfs_mount() {
	let env = TestEnv::new();
	env.init_db().await;
	// a project mount with an explicit `/tmp` dest collides with the built-in tmpfs.
	let proj = tempfile::tempdir().unwrap();
	let src = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		format!(
			r#"{{"mounts": [{{"src": "{}", "dest": "/tmp", "mode": "ro"}}]}}"#,
			src.path().display()
		),
	)
	.unwrap();
	common::expect_stderr_contains(
		&env,
		&["create", "alpha", proj.path().to_str().unwrap()],
		"conflict: '/tmp'",
	);
}

#[tokio::test]
async fn create_user_mount_cannot_shadow_tmpfs() {
	let env = TestEnv::new();
	env.init_db().await;
	// a directory named `tmp` collides with the /tmp tmpfs dest.
	// a differently-named directory must not.
	let dir = tempfile::tempdir().unwrap();
	let tmpdir = dir.path().join("tmpdir");
	std::fs::create_dir(&tmpdir).unwrap();
	let config = env.create_config(&["alpha", "--rw", tmpdir.to_str().unwrap()]);
	assert_eq!(config["runtime"]["workdir"], "/home/agent");
}

#[tokio::test]
async fn create_replace_is_accepted() {
	let env = TestEnv::new();
	env.init_db().await;
	env.run_ok(true, &["create", "alpha", "--replace"]);
}

#[tokio::test]
async fn create_missing_user_config_file_fails() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(
		&env,
		&["create", "alpha", "--conf", "/does/not/exist.yml"],
		"missing config file",
	);
}

#[tokio::test]
async fn create_merges_project_svamm_json_rules() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"network": {"allow": ["svamm-project.test:tcp:443"]}}"#,
	)
	.unwrap();
	let config = env.create_config(&["alpha", proj.path().to_str().unwrap()]);
	let rules = config["network"]["policy"]["rules"].as_array().unwrap();
	let has_project_rule = rules.iter().any(|rule| {
		rule["destination"].get("domain").and_then(|d| d.as_str()) == Some("svamm-project.test")
	});
	assert!(
		has_project_rule,
		"project rule must be merged into the policy"
	);
}

#[tokio::test]
async fn create_always_generates_registry_host_rule() {
	let env = TestEnv::new();
	env.init_db().await;
	// an emptied net-rules file must not remove registry access.
	// the `host:tcp:<port>` rule is structural and always generated.
	std::fs::write(
		env.config_dir().join("net-rules.json"),
		r#"{"network": {"allow": [], "deny": []}}"#,
	)
	.unwrap();
	let proj = tempfile::tempdir().unwrap();
	let config = env.create_config(&["alpha", proj.path().to_str().unwrap()]);
	assert_eq!(config["network"]["policy"]["default_egress"], "deny");
	let rules = config["network"]["policy"]["rules"].as_array().unwrap();
	let has_registry_rule = rules.iter().any(|rule| {
		rule["direction"] == "egress"
			&& rule["action"] == "allow"
			&& rule["destination"].get("group").and_then(|g| g.as_str()) == Some("host")
			&& rule["protocols"]
				.as_array()
				.is_some_and(|protos| protos.iter().any(|p| p == "tcp"))
			&& rule["ports"].as_array().is_some_and(|ports| {
				ports.iter().any(|range| {
					range["start"] == serde_json::json!(env.proxy_port)
						&& range["end"] == serde_json::json!(env.proxy_port)
				})
			})
	});
	assert!(
		has_registry_rule,
		"registry host rule for proxy port {} must be generated",
		env.proxy_port
	);
}

#[tokio::test]
async fn create_skips_project_rules_when_main_mount_has_no_svamm_json() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	let config = env.create_config(&["alpha", proj.path().to_str().unwrap()]);
	let rules = config["network"]["policy"]["rules"].as_array().unwrap();
	assert!(
		rules.iter().all(
			|rule| rule["destination"].get("domain").and_then(|d| d.as_str())
				!= Some("svamm-project.test")
		),
		"no project rule should be present"
	);
}

#[tokio::test]
async fn create_skips_project_rules_when_main_mount_is_a_file() {
	let env = TestEnv::new();
	env.init_db().await;
	let file = tempfile::NamedTempFile::new().unwrap();
	let config = env.create_config(&["alpha", file.path().to_str().unwrap()]);
	let rules = config["network"]["policy"]["rules"].as_array().unwrap();
	assert!(
		rules.iter().all(
			|rule| rule["destination"].get("domain").and_then(|d| d.as_str())
				!= Some("svamm-project.test")
		),
		"no project rule should be present"
	);
}

#[tokio::test]
async fn create_applies_project_ports() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"network": {"allow": [], "ports": ["9000:80", "0.0.0.0:8081:81/udp"]}}"#,
	)
	.unwrap();
	let config = env.create_config(&["alpha", proj.path().to_str().unwrap()]);
	let ports = config["network"]["ports"].as_array().unwrap();
	assert_eq!(
		ports[0],
		serde_json::json!({
			"host_port": 9000,
			"guest_port": 80,
			"protocol": "tcp",
			"host_bind": "127.0.0.1",
		})
	);
	assert_eq!(
		ports[1],
		serde_json::json!({
			"host_port": 8081,
			"guest_port": 81,
			"protocol": "udp",
			"host_bind": "0.0.0.0",
		})
	);
}

#[tokio::test]
async fn create_applies_project_ports_top_level_sugar() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(proj.path().join("svamm.json"), r#"{"ports": ["9000:80"]}"#).unwrap();
	let config = env.create_config(&["alpha", proj.path().to_str().unwrap()]);
	let ports = config["network"]["ports"].as_array().unwrap();
	assert_eq!(ports.len(), 1);
	assert_eq!(ports[0]["host_port"], 9000);
	assert_eq!(ports[0]["guest_port"], 80);
}

#[tokio::test]
async fn create_ports_only_svamm_json_no_network() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"network": {"ports": ["9000:80"]}}"#,
	)
	.unwrap();
	let config = env.create_config(&["alpha", proj.path().to_str().unwrap()]);
	let ports = config["network"]["ports"].as_array().unwrap();
	assert_eq!(ports.len(), 1);
	assert_eq!(ports[0]["host_port"], 9000);
	assert_eq!(ports[0]["guest_port"], 80);
}

#[tokio::test]
async fn create_invalid_project_port_fails() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"network": {"ports": ["9000"]}}"#,
	)
	.unwrap();
	let (stderr, _) = env.run_fail(true, &["create", "alpha", proj.path().to_str().unwrap()]);
	assert!(
		stderr.contains("expected HOST:GUEST"),
		"unexpected stderr: {stderr}"
	);
}

#[tokio::test]
async fn create_no_project_ports_leaves_ports_empty() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env.create_config(&["alpha"]);
	assert!(
		config["network"]["ports"].as_array().unwrap().is_empty(),
		"no ports should be published without a project svamm.json"
	);
}

#[tokio::test]
async fn create_root_disk_size_is_passed_through() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env.create_config(&["alpha", "--root-disk-size", "32G"]);
	assert_eq!(config["image"]["Oci"]["root_disk"]["size_mib"], 32768);
}

#[tokio::test]
async fn create_root_disk_size_accepts_plain_numbers() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env.create_config(&["alpha", "--root-disk-size", "2048M"]);
	assert_eq!(config["image"]["Oci"]["root_disk"]["size_mib"], 2048);
}

#[tokio::test]
async fn create_root_disk_size_env_var_sets_default() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env
		.cmd(true)
		.env("SVAMM_ROOT_DISK_SIZE", "8G")
		.args(["create", "alpha"])
		.output()
		.unwrap();
	assert!(config.status.success());
	let parsed: serde_json::Value = serde_json::from_slice(&config.stdout).unwrap();
	assert_eq!(parsed["image"]["Oci"]["root_disk"]["size_mib"], 8192);
}

#[tokio::test]
async fn create_root_disk_size_flag_overrides_env() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env
		.cmd(true)
		.env("SVAMM_ROOT_DISK_SIZE", "8G")
		.args(["create", "alpha", "--root-disk-size", "32G"])
		.output()
		.unwrap();
	assert!(config.status.success());
	let parsed: serde_json::Value = serde_json::from_slice(&config.stdout).unwrap();
	assert_eq!(parsed["image"]["Oci"]["root_disk"]["size_mib"], 32768);
}

#[tokio::test]
async fn create_invalid_root_disk_size_fails() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(
		&env,
		&["create", "alpha", "--root-disk-size", "banana"],
		"invalid root disk size 'banana'",
	);
}

#[tokio::test]
async fn create_invalid_env_root_disk_size_fails() {
	let env = TestEnv::new();
	env.init_db().await;
	let output = env
		.cmd(true)
		.env("SVAMM_ROOT_DISK_SIZE", "banana")
		.args(["create", "alpha"])
		.output()
		.unwrap();
	assert!(!output.status.success());
	assert!(
		String::from_utf8_lossy(&output.stderr).contains("invalid root disk size 'banana'"),
		"unexpected stderr: {}",
		String::from_utf8_lossy(&output.stderr)
	);
}

#[tokio::test]
async fn create_tmpfs_size_flag_sets_size() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env.create_config(&["alpha", "--tmpfs-size", "4G"]);
	let tmpfs = mount_by_guest(&config, "/tmp").expect("tmpfs mount");
	assert_eq!(tmpfs["type"], "Tmpfs");
	assert_eq!(tmpfs["size_mib"], 4096);
}

#[tokio::test]
async fn create_tmpfs_size_env_var_sets_default() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env
		.cmd(true)
		.env("SVAMM_TMPFS_SIZE", "6G")
		.args(["create", "alpha"])
		.output()
		.unwrap();
	assert!(config.status.success());
	let parsed: serde_json::Value = serde_json::from_slice(&config.stdout).unwrap();
	let tmpfs = mount_by_guest(&parsed, "/tmp").expect("tmpfs mount");
	assert_eq!(tmpfs["size_mib"], 6144);
}

#[tokio::test]
async fn create_tmpfs_size_flag_overrides_env() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env
		.cmd(true)
		.env("SVAMM_TMPFS_SIZE", "6G")
		.args(["create", "alpha", "--tmpfs-size", "4G"])
		.output()
		.unwrap();
	assert!(config.status.success());
	let parsed: serde_json::Value = serde_json::from_slice(&config.stdout).unwrap();
	let tmpfs = mount_by_guest(&parsed, "/tmp").expect("tmpfs mount");
	assert_eq!(tmpfs["size_mib"], 4096);
}

#[tokio::test]
async fn create_invalid_tmpfs_size_fails() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(
		&env,
		&["create", "alpha", "--tmpfs-size", "banana"],
		"invalid tmpfs size 'banana'",
	);
}

#[tokio::test]
async fn create_burstable_resources_are_applied() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env.create_config(&[
		"alpha",
		"--cpus",
		"4",
		"--memory",
		"8G",
		"--max-cpus",
		"8",
		"--max-memory",
		"16G",
	]);
	assert_eq!(config["resources"]["cpus"], 4);
	assert_eq!(config["resources"]["memory_mib"], 8192);
	assert_eq!(config["resources"]["max_cpus"], 8);
	assert_eq!(config["resources"]["max_memory_mib"], 16384);
}

#[tokio::test]
async fn create_burstable_config_env_is_applied() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env
		.cmd(true)
		.env("SVAMM_CPUS", "4")
		.env("SVAMM_MEMORY", "8G")
		.env("SVAMM_MAX_CPUS", "8")
		.env("SVAMM_MAX_MEMORY", "16G")
		.args(["create", "alpha"])
		.output()
		.unwrap();
	assert!(config.status.success());
	let parsed: serde_json::Value = serde_json::from_slice(&config.stdout).unwrap();
	assert_eq!(parsed["resources"]["cpus"], 4);
	assert_eq!(parsed["resources"]["memory_mib"], 8192);
	assert_eq!(parsed["resources"]["max_cpus"], 8);
	assert_eq!(parsed["resources"]["max_memory_mib"], 16384);
}

#[tokio::test]
async fn create_resource_flags_override_config_env() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env
		.cmd(true)
		.env("SVAMM_CPUS", "2")
		.env("SVAMM_MAX_CPUS", "16")
		.args(["create", "alpha", "--cpus", "4", "--max-cpus", "8"])
		.output()
		.unwrap();
	assert!(config.status.success());
	let parsed: serde_json::Value = serde_json::from_slice(&config.stdout).unwrap();
	assert_eq!(parsed["resources"]["cpus"], 4);
	assert_eq!(parsed["resources"]["max_cpus"], 8);
}

#[tokio::test]
async fn create_invalid_memory_size_fails() {
	let env = TestEnv::new();
	env.init_db().await;
	common::expect_stderr_contains(
		&env,
		&["create", "alpha", "--memory", "banana"],
		"invalid memory size 'banana'",
	);
	common::expect_stderr_contains(
		&env,
		&["create", "alpha", "--max-memory", "1Gx"],
		"invalid memory size '1Gx'",
	);
}

#[tokio::test]
async fn create_no_flat_omits_flat_root_disk() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env.create_config(&["alpha", "--no-flat", "--root-disk-size", "32G"]);
	// without `--root-disk`, the sdk leaves the root disk unset.
	assert_ne!(config["image"]["Oci"]["root_disk"]["kind"], "flat");
}

#[tokio::test]
async fn create_no_mounts_yields_agent_home_workdir() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env.create_config(&["beta"]);
	assert_eq!(config["runtime"]["workdir"], "/home/agent");
}

#[tokio::test]
async fn create_user_resource_conf_is_applied() {
	let env = TestEnv::new();
	env.init_db().await;
	let conf = tempfile::NamedTempFile::new().unwrap();
	std::fs::write(conf.path(), "cpus: 4\nmemory: \"2G\"\n").unwrap();
	let config = env.create_config(&["alpha", "--resource-conf", conf.path().to_str().unwrap()]);
	assert_eq!(config["resources"]["cpus"], 4);
	assert_eq!(config["resources"]["memory_mib"], 2048);
}

#[tokio::test]
async fn create_user_fs_conf_is_applied() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	let fs_conf = tempfile::NamedTempFile::new().unwrap();
	std::fs::write(
		fs_conf.path(),
		format!(
			"mounts:\n  - bind: {}\n    target: /workspace\n    readonly: true\n",
			proj.path().display()
		),
	)
	.unwrap();
	let config = env.create_config(&["alpha", "--fs-conf", fs_conf.path().to_str().unwrap()]);
	let mount = mount_by_guest(&config, "/workspace").expect("fs-conf mount");
	assert_eq!(mount["options"]["readonly"], true);
}

#[tokio::test]
async fn create_base_defaults_are_applied() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env.create_config(&["alpha"]);
	let env_entries = config["env"].as_array().unwrap();
	assert!(
		env_entries
			.iter()
			.any(|entry| entry["key"] == "TERM" && entry["value"] == "xterm-256color"),
		"base.yml env must be applied"
	);
	assert_eq!(config["init"]["cmd"], "auto");
	assert_eq!(config["resources"]["cpus"], 16);
	assert_eq!(config["resources"]["memory_mib"], 16384);
}

#[tokio::test]
async fn create_reports_success_on_stdout() {
	// non-dry create needs a registry and a bootable vm. without kvm,
	// create fails at spawn. point both image refs at a local fake oci
	// registry so the full flow runs hermetically and fast.
	let mut env = TestEnv::new();
	env.init_db().await;
	let fake = common::start_fake_oci_registry();
	env.port = fake.port;
	env.mark_msb_insecure(&format!("127.0.0.1:{}", fake.port));

	let reference = format!("127.0.0.1:{}/svamm:latest", fake.port);
	let output = env
		.cmd(false)
		.env("SVAMM_IMAGES_VM", &reference)
		.env("SVAMM_IMAGES_REGISTRY", &reference)
		.arg("create")
		.arg("alpha")
		.output()
		.unwrap();
	assert!(
		!output.status.success(),
		"non-dry create must fail without KVM; stdout:\n{}\nstderr:\n{}",
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr)
	);
}

#[tokio::test]
async fn create_mounts_are_present() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env.create_config(&["alpha"]);
	let all = mounts(&config);
	assert!(
		all.iter().any(|m| m["guest"] == "/tmp"),
		"tmpfs mount missing"
	);
	// no project file and no cli mounts. nothing else is mounted.
	assert_eq!(
		all.iter()
			.filter(|m| m["guest"].as_str() != Some("/tmp"))
			.count(),
		0,
		"only the built-in tmpfs should be mounted without a project or flags"
	);
}

#[tokio::test]
async fn create_applies_project_mounts_mixed_modes() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	let work = tempfile::tempdir().unwrap();
	let ref_a = tempfile::tempdir().unwrap();
	let ref_b = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		format!(
			r#"{{"mounts": [
				{{"src": "{}", "dest": "/work", "mode": "rw"}},
				{{"src": "{}", "mode": "ro"}},
				{{"src": "{}", "dest": "/custom/ref", "mode": "ro"}}
			]}}"#,
			work.path().display(),
			ref_a.path().display(),
			ref_b.path().display()
		),
	)
	.unwrap();
	let config = env.create_config(&["alpha", proj.path().to_str().unwrap()]);

	let work_mount = mount_by_guest(&config, "/work").expect("project rw mount");
	assert_eq!(work_mount["options"]["readonly"], false);
	assert_eq!(work_mount["host"], work.path().to_str().unwrap());
	// rw mounts carry the guest-write quota.
	assert_eq!(work_mount["quota_mib"], 32768);
	// an entry without a dest auto-derives /home/agent/<basename>.
	let ref_a_guest = format!(
		"/home/agent/{}",
		ref_a.path().file_name().unwrap().to_string_lossy()
	);
	let ref_a_mount = mount_by_guest(&config, &ref_a_guest).expect("project ro mount (auto dest)");
	assert_eq!(ref_a_mount["options"]["readonly"], true);
	assert_eq!(ref_a_mount["host"], ref_a.path().to_str().unwrap());
	assert!(ref_a_mount["quota_mib"].is_null());
	let ref_b_mount = mount_by_guest(&config, "/custom/ref").expect("project ro mount");
	assert_eq!(ref_b_mount["options"]["readonly"], true);
	assert_eq!(ref_b_mount["host"], ref_b.path().to_str().unwrap());
}

#[tokio::test]
async fn create_project_mount_mode_is_respected_per_entry() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	let src = tempfile::tempdir().unwrap();
	// the same host path declared rw and ro must yield two mounts
	// with distinct guest dests and modes.
	std::fs::write(
		proj.path().join("svamm.json"),
		format!(
			r#"{{"mounts": [
				{{"src": "{}", "dest": "/same/rw", "mode": "rw"}},
				{{"src": "{}", "dest": "/same/ro", "mode": "ro"}}
			]}}"#,
			src.path().display(),
			src.path().display()
		),
	)
	.unwrap();
	let config = env.create_config(&["alpha", proj.path().to_str().unwrap()]);
	let rw_mount = mount_by_guest(&config, "/same/rw").expect("rw mount");
	assert_eq!(rw_mount["options"]["readonly"], false);
	assert_eq!(rw_mount["quota_mib"], 32768);
	let ro_mount = mount_by_guest(&config, "/same/ro").expect("ro mount");
	assert_eq!(ro_mount["options"]["readonly"], true);
	assert!(ro_mount["quota_mib"].is_null());
}

#[tokio::test]
async fn create_project_mounts_merge_with_cli_flags() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	let proj_ref = tempfile::tempdir().unwrap();
	let cli_ref = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		format!(
			r#"{{"mounts": [{{"src": "{}", "mode": "ro"}}]}}"#,
			proj_ref.path().display()
		),
	)
	.unwrap();
	let config = env.create_config(&[
		"alpha",
		proj.path().to_str().unwrap(),
		"--ro",
		cli_ref.path().to_str().unwrap(),
	]);

	let proj_guest = format!(
		"/home/agent/{}",
		proj_ref.path().file_name().unwrap().to_string_lossy()
	);
	let cli_guest = format!(
		"/home/agent/{}",
		cli_ref.path().file_name().unwrap().to_string_lossy()
	);
	assert_ne!(proj_guest, cli_guest, "test requires distinct guest dests");
	assert!(
		mount_by_guest(&config, &proj_guest).is_some(),
		"project ro mount must be present alongside CLI flags"
	);
	assert!(
		mount_by_guest(&config, &cli_guest).is_some(),
		"CLI ro mount must be present alongside project mounts"
	);
}

#[tokio::test]
async fn create_project_mounts_conflict_with_cli_flag() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	let parent_a = tempfile::tempdir().unwrap();
	let parent_b = tempfile::tempdir().unwrap();
	let proj_ref = parent_a.path().join("dup");
	let cli_ref = parent_b.path().join("dup");
	std::fs::create_dir(&proj_ref).unwrap();
	std::fs::create_dir(&cli_ref).unwrap();
	// the project ro mount and the cli ro mount derive the same guest
	// destination from the shared basename.
	std::fs::write(
		proj.path().join("svamm.json"),
		format!(
			r#"{{"mounts": [{{"src": "{}", "mode": "ro"}}]}}"#,
			proj_ref.display()
		),
	)
	.unwrap();
	common::expect_stderr_contains(
		&env,
		&[
			"create",
			"alpha",
			proj.path().to_str().unwrap(),
			"--ro",
			cli_ref.to_str().unwrap(),
		],
		"mount destination conflict: '/home/agent/dup'",
	);
}

#[tokio::test]
async fn create_without_project_mounts_adds_none() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"network": {"allow": ["svamm-project.test:tcp:443"]}}"#,
	)
	.unwrap();
	let config = env.create_config(&["alpha", proj.path().to_str().unwrap()]);
	// only the built-in mounts are present. the absent `mounts` key adds nothing.
	let base = format!(
		"/home/agent/{}",
		proj.path().file_name().unwrap().to_string_lossy()
	);
	let overlay = format!("{base}/svamm.json");
	let expected: Vec<String> = vec!["/tmp".to_string(), base.clone(), overlay];
	let guests: Vec<String> = mounts(&config)
		.iter()
		.filter_map(|m| m["guest"].as_str().map(str::to_string))
		.collect();
	assert_eq!(guests, expected);
}

#[tokio::test]
async fn create_project_mounts_conflict_with_main_mount() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	let ref_src = tempfile::tempdir().unwrap();
	let main_guest = format!(
		"/home/agent/{}",
		proj.path().file_name().unwrap().to_string_lossy()
	);
	std::fs::write(
		proj.path().join("svamm.json"),
		format!(
			r#"{{"mounts": [{{"src": "{}", "dest": "{}", "mode": "ro"}}]}}"#,
			ref_src.path().display(),
			main_guest
		),
	)
	.unwrap();
	// the project ro mount dest collides with the main mount's guest dest.
	common::expect_stderr_contains(
		&env,
		&["create", "alpha", proj.path().to_str().unwrap()],
		&format!("mount destination conflict: '{main_guest}'"),
	);
}

#[tokio::test]
async fn create_invalid_project_mounts_fails() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	// the "mode" key is missing.
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"mounts": [{"src": "/srv/ref"}]}"#,
	)
	.unwrap();
	common::expect_stderr_contains(
		&env,
		&["create", "alpha", proj.path().to_str().unwrap()],
		"requires a non-empty \"mode\"",
	);
}

#[tokio::test]
async fn create_project_mounts_rejects_bad_mode() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"mounts": [{"src": "/srv/ref", "mode": "rwe"}]}"#,
	)
	.unwrap();
	common::expect_stderr_contains(
		&env,
		&["create", "alpha", proj.path().to_str().unwrap()],
		"\"mode\" must be \"rw\" or \"ro\", got \"rwe\"",
	);
}

#[tokio::test]
async fn create_project_mounts_rejects_string_entry() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"mounts": ["/srv/ref"]}"#,
	)
	.unwrap();
	common::expect_stderr_contains(
		&env,
		&["create", "alpha", proj.path().to_str().unwrap()],
		"must be an object",
	);
}

#[tokio::test]
async fn create_project_mounts_missing_source_fails() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"mounts": [{"src": "/definitely/not/here", "mode": "ro"}]}"#,
	)
	.unwrap();
	common::expect_stderr_contains(
		&env,
		&["create", "alpha", proj.path().to_str().unwrap()],
		"mount source does not exist",
	);
}

#[tokio::test]
async fn create_applies_project_image_override() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"image": "127.0.0.1:50000/acme-toolchain:latest"}"#,
	)
	.unwrap();
	let config = env.create_config(&["alpha", proj.path().to_str().unwrap()]);
	assert_eq!(
		config["image"]["Oci"]["reference"],
		"127.0.0.1:50000/acme-toolchain:latest"
	);
}

#[tokio::test]
async fn create_project_image_empty_falls_back_to_default() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(proj.path().join("svamm.json"), r#"{"image": ""}"#).unwrap();
	let config = env.create_config(&["alpha", proj.path().to_str().unwrap()]);
	assert_eq!(
		config["image"]["Oci"]["reference"],
		"ghcr.io/intfish/svamm-images/svamm:latest"
	);
}

#[tokio::test]
async fn create_uses_configured_images_vm_env() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env
		.cmd(true)
		.env("SVAMM_IMAGES_VM", "127.0.0.1:50000/acme:latest")
		.args(["create", "alpha"])
		.output()
		.unwrap();
	assert!(config.status.success());
	let parsed: serde_json::Value = serde_json::from_slice(&config.stdout).unwrap();
	assert_eq!(
		parsed["image"]["Oci"]["reference"],
		"127.0.0.1:50000/acme:latest"
	);
}

#[tokio::test]
async fn create_passes_registry_proxy_port_env() {
	let env = TestEnv::new();
	env.init_db().await;
	let config = env
		.cmd(true)
		.env("SVAMM_REGISTRY_PROXY_PORT", "51111")
		.args(["create", "alpha"])
		.output()
		.unwrap();
	assert!(config.status.success());
	let parsed: serde_json::Value = serde_json::from_slice(&config.stdout).unwrap();
	let envs: Vec<String> = parsed["env"]
		.as_array()
		.unwrap()
		.iter()
		.map(|e| {
			format!(
				"{}={}",
				e["key"].as_str().unwrap_or(""),
				e["value"].as_str().unwrap_or("")
			)
		})
		.collect();
	assert!(
		envs.contains(&"SVAMM_REGISTRY_PROXY_PORT=51111".to_string()),
		"sandbox must receive the proxy port for the daemon.json renderer; got: {envs:?}"
	);
}

#[tokio::test]
async fn create_invalid_project_image_fails() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"image": "not a ref!!"}"#,
	)
	.unwrap();
	common::expect_stderr_contains(
		&env,
		&["create", "alpha", proj.path().to_str().unwrap()],
		"invalid image reference 'not a ref!!'",
	);
}

#[tokio::test]
async fn create_project_image_non_string_fails() {
	let env = TestEnv::new();
	env.init_db().await;
	let proj = tempfile::tempdir().unwrap();
	std::fs::write(
		proj.path().join("svamm.json"),
		r#"{"image": {"reference": "127.0.0.1:50000/acme:latest"}}"#,
	)
	.unwrap();
	common::expect_stderr_contains(
		&env,
		&["create", "alpha", proj.path().to_str().unwrap()],
		"\"image\" must be a string",
	);
}
