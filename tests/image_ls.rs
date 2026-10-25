//! `svamm image ls` tests against a fake docker-registry http server.
//!
//! the local registry is served by the shared fake (catalog and per-repo
//! tags/list). the registry-down case uses no server and a seeded registry
//! sandbox, so the best-effort bring-up is a no-op.

mod common;

use common::TestEnv;

#[tokio::test]
async fn image_ls_lists_all_repositories_and_tags_with_pull_uri() {
	let mut env = TestEnv::new();
	env.init_db().await;
	env.start_fake_catalog_registry(&[
		("svamm", &[("latest", "sha256:a"), ("old-1", "sha256:d")]),
		("acme/agent", &[("1.2.3", "sha256:b")]),
	]);

	let stdout = env.run_ok(false, &["image", "ls"]);
	for header in ["REPOSITORY", "TAG", "SIZE", "PULL"] {
		assert!(stdout.contains(header), "missing {header} header: {stdout}");
	}
	// the fake serves a fixed 2048-byte manifest for every tag.
	assert_eq!(
		stdout.matches("2 KB").count(),
		3,
		"every image must carry a size: {stdout}"
	);
	// the pull uri is the sandbox-facing one through the read-only proxy.
	for expected in [
		&format!("svamm.internal:{}/svamm:latest", env.proxy_port),
		&format!("svamm.internal:{}/svamm:old-1", env.proxy_port),
		&format!("svamm.internal:{}/acme/agent:1.2.3", env.proxy_port),
	] {
		assert!(
			stdout.contains(expected),
			"missing pull uri {expected}: {stdout}"
		);
	}
	// each image is its own row (repo, tag, pull uri).
	let lines: Vec<&str> = stdout.lines().collect();
	let svamm_rows: Vec<&str> = lines
		.iter()
		.filter(|line| line.contains("svamm") && !line.contains("acme/agent"))
		.cloned()
		.collect();
	assert_eq!(svamm_rows.len(), 2, "one row per svamm tag: {stdout}");
	assert!(svamm_rows[0].contains("latest") && !svamm_rows[0].contains("old-1"));
	assert!(svamm_rows[1].contains("old-1") && !svamm_rows[1].contains("latest"));
	assert!(
		stdout.contains(&format!(
			"svamm.internal:{}/acme/agent:1.2.3",
			env.proxy_port
		)),
		"mirrored repo must include its pull uri: {stdout}"
	);
}

#[tokio::test]
async fn image_ls_alias_matches_list() {
	let mut env = TestEnv::new();
	env.init_db().await;
	env.start_fake_catalog_registry(&[("svamm", &[("latest", "sha256:a")])]);

	let list = env.run_ok(false, &["image", "list"]);
	let ls = env.run_ok(false, &["image", "ls"]);
	assert_eq!(
		list.trim(),
		ls.trim(),
		"`image ls` must mirror `image list`"
	);
	assert!(
		ls.contains(&format!("svamm.internal:{}/svamm:latest", env.proxy_port)),
		"missing pull uri; got: {ls}"
	);
}

#[tokio::test]
async fn image_ls_empty_registry_reports_note() {
	let mut env = TestEnv::new();
	env.init_db().await;
	env.start_fake_catalog_registry(&[]);

	let stdout = env.run_ok(false, &["image", "ls"]);
	assert!(
		stdout.contains("no images in the local registry"),
		"expected empty note; got: {stdout}"
	);
}

#[tokio::test]
async fn image_ls_skips_repositories_without_tags() {
	// `bare` has no tags left. it lingers in the catalog as an empty shell
	// and must not produce a `-` row.
	let mut env = TestEnv::new();
	env.init_db().await;
	env.start_fake_catalog_registry(&[("svamm", &[("latest", "sha256:a")]), ("bare", &[])]);

	let stdout = env.run_ok(false, &["image", "ls"]);
	assert!(
		stdout.contains(&format!("svamm.internal:{}/svamm:latest", env.proxy_port)),
		"tagged image must be listed; got: {stdout}"
	);
	assert!(
		!stdout
			.lines()
			.any(|line| line.split_whitespace().next() == Some("bare")),
		"empty repository must be hidden; got: {stdout}"
	);
}

#[tokio::test]
async fn image_ls_only_empty_repositories_reports_note() {
	let mut env = TestEnv::new();
	env.init_db().await;
	env.start_fake_catalog_registry(&[("bare1", &[]), ("bare2", &[])]);

	let stdout = env.run_ok(false, &["image", "ls"]);
	assert!(
		stdout.contains("no images in the local registry"),
		"only-empty registry must report the empty note; got: {stdout}"
	);
	assert!(!stdout.contains("bare1") && !stdout.contains("bare2"));
}

#[tokio::test]
async fn image_ls_registry_down_warns_with_hint() {
	// no fake server. the catalog query fails. the best-effort bring-up
	// finds the seeded registry sandbox running, waits the bounded readiness
	// timeout (shrunk by the harness), then warns instead of hanging.
	let env = TestEnv::new();
	env.init_db().await;
	env.seed_registry().await;

	let stdout = env.run_ok(false, &["image", "ls"]);
	assert!(
		stdout.contains("not reachable"),
		"expected unreachable warning; got: {stdout}"
	);
	assert!(
		stdout.contains("svamm setup"),
		"expected repair hint; got: {stdout}"
	);
}

#[tokio::test]
async fn image_ls_waits_for_late_registry() {
	// the fake answers 500 for the first three requests, then serves
	// normally. the first catalog probe fails; `svamm image ls` must wait
	// for readiness instead of giving up after one retry. the seeded "Running"
	// sandbox keeps the bring-up a no-op. three failures guarantee the probe
	// and the wait both land on errors before the server recovers.
	let mut env = TestEnv::new();
	env.init_db().await;
	env.seed_registry().await;
	env.start_slow_catalog_registry(&[("svamm", &[("latest", "sha256:a")])], 3);

	let stdout = env.run_ok(false, &["image", "ls"]);
	assert!(
		stdout.contains(&format!("svamm.internal:{}/svamm:latest", env.proxy_port)),
		"late registry must still be listed; got: {stdout}"
	);
	assert!(
		!stdout.contains("not reachable"),
		"no unreachable warning expected once the registry answers; got: {stdout}"
	);
}

#[test]
fn image_pull_dry_run_skips_docker() {
	// dry-run must not touch the docker daemon or the registry, and must not
	// fail when the vm image points at the (possibly stopped) local registry.
	let env = TestEnv::new();
	let stdout = env.run_ok(true, &["image", "pull"]);
	assert!(
		stdout.contains("would pull ghcr.io/intfish/svamm-images/svamm:latest"),
		"dry-run must preview the pull; got: {stdout}"
	);
	assert!(
		stdout.contains("svamm:latest"),
		"dry-run must preview the local tag; got: {stdout}"
	);
}
