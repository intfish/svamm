//! `svamm image rm` tests against a fake docker-registry http server and a
//! seeded msb cache database.
//!
//! removal is refused while any existing sandbox references the image's
//! manifest digest. otherwise the manifest is deleted from the registry and
//! the matching msb cache reference is removed.

mod common;

use common::{oci_config, TestEnv, DIGEST_D, DIGEST_E};

#[test]
fn image_rm_deletes_unreferenced_image_from_registry_and_cache() {
	let mut env = TestEnv::new();
	let runtime = tokio::runtime::Runtime::new().unwrap();
	runtime.block_on(async {
		env.init_db().await;
		env.seed_registry().await;
	});
	let registry = env.start_fake_registry(&[("old-1", DIGEST_D)]);

	let cache_ref = format!("127.0.0.1:{}/svamm:old-1", env.port);
	runtime.block_on(async {
		env.seed_cached_image(&cache_ref, DIGEST_D).await;
	});

	let stdout = env.run_ok(false, &["image", "rm", "svamm:old-1"]);
	assert!(
		stdout.contains("deleted registry image svamm:old-1"),
		"missing registry delete; got:\n{stdout}"
	);
	assert!(
		stdout.contains(&format!("removed cached image {cache_ref}")),
		"missing cache removal; got:\n{stdout}"
	);

	assert!(
		registry
			.deleted
			.lock()
			.unwrap()
			.contains(&DIGEST_D.to_string()),
		"unreferenced digest must be deleted from the registry"
	);
	let db = runtime.block_on(env.db());
	let remaining: Vec<String> = runtime.block_on(async {
		sqlx::query_scalar("SELECT reference FROM image_ref")
			.fetch_all(&db)
			.await
			.unwrap()
	});
	assert!(
		!remaining.contains(&cache_ref),
		"cached reference must be gone: {remaining:?}"
	);
}

#[test]
fn image_rm_dry_run_previews_without_deleting() {
	let mut env = TestEnv::new();
	let runtime = tokio::runtime::Runtime::new().unwrap();
	runtime.block_on(async {
		env.init_db().await;
		env.seed_registry().await;
	});
	let registry = env.start_fake_registry(&[("old-1", DIGEST_D)]);
	let cache_ref = format!("127.0.0.1:{}/svamm:old-1", env.port);
	runtime.block_on(async {
		env.seed_cached_image(&cache_ref, DIGEST_D).await;
	});

	let stdout = env.run_ok(true, &["image", "rm", "svamm:old-1"]);
	assert!(
		stdout.contains("would delete registry image svamm:old-1 (sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd)"),
		"dry-run must preview the digest; got:\n{stdout}"
	);
	assert!(
		stdout.contains("would remove cached image") && stdout.contains("svamm:old-1"),
		"dry-run must preview cache removal; got:\n{stdout}"
	);
	assert!(
		registry.deleted.lock().unwrap().is_empty(),
		"dry-run must not delete manifests"
	);
	let db = runtime.block_on(env.db());
	let count: i64 = runtime.block_on(async {
		sqlx::query_scalar("SELECT COUNT(*) FROM image_ref")
			.fetch_one(&db)
			.await
			.unwrap()
	});
	assert_eq!(count, 1, "dry-run must not remove cache refs");
}

#[test]
fn image_rm_refuses_image_when_a_sandbox_pins_it() {
	let mut env = TestEnv::new();
	let runtime = tokio::runtime::Runtime::new().unwrap();
	runtime.block_on(async {
		env.init_db().await;
		env.seed_registry().await;
		// alpha boots from DIGEST_D, exactly what svamm:old-1 resolves to.
		env.seed_sandbox(
			"alpha",
			"Running",
			Some(("svamm", "managed")),
			oci_config(env.port, "alpha", "/home/agent/proj", DIGEST_D),
		)
		.await;
	});
	let registry = env.start_fake_registry(&[("old-1", DIGEST_D)]);
	let cache_ref = format!("127.0.0.1:{}/svamm:old-1", env.port);
	runtime.block_on(async {
		env.seed_cached_image(&cache_ref, DIGEST_D).await;
	});

	let stdout = env.run_ok(false, &["image", "rm", "svamm:old-1"]);
	assert!(
		stdout.contains("is in use by alpha"),
		"expected the pinning sandbox to be named; got:\n{stdout}"
	);
	assert!(
		stdout.contains("nothing removed"),
		"expected nothing-removed warning; got:\n{stdout}"
	);
	assert!(
		!stdout.contains("deleted registry image"),
		"no registry delete expected; got:\n{stdout}"
	);
	assert!(
		registry.deleted.lock().unwrap().is_empty(),
		"in-use image must not be deleted from the registry"
	);
	let db = runtime.block_on(env.db());
	let remaining: Vec<String> = runtime.block_on(async {
		sqlx::query_scalar("SELECT reference FROM image_ref")
			.fetch_all(&db)
			.await
			.unwrap()
	});
	assert!(
		remaining.contains(&cache_ref),
		"cached copy must be kept while in use: {remaining:?}"
	);
}

#[test]
fn image_rm_removes_mirrored_repo_image() {
	let mut env = TestEnv::new();
	let runtime = tokio::runtime::Runtime::new().unwrap();
	runtime.block_on(async {
		env.init_db().await;
		env.seed_registry().await;
	});
	let registry = env.start_fake_catalog_registry(&[("acme/agent", &[("1.2.3", DIGEST_E)])]);
	let cache_ref = format!("127.0.0.1:{}/acme/agent:1.2.3", env.port);
	runtime.block_on(async {
		env.seed_cached_image(&cache_ref, DIGEST_E).await;
	});

	let stdout = env.run_ok(false, &["image", "rm", "acme/agent:1.2.3"]);
	assert!(
		stdout.contains("deleted registry image acme/agent:1.2.3"),
		"missing registry delete; got:\n{stdout}"
	);
	assert!(
		stdout.contains(&format!("removed cached image {cache_ref}")),
		"missing cache removal; got:\n{stdout}"
	);
	assert!(
		registry
			.deleted
			.lock()
			.unwrap()
			.contains(&DIGEST_E.to_string()),
		"mirrored digest must be deleted from the registry"
	);
}

#[test]
fn image_rm_removing_last_tag_hides_the_repository() {
	let mut env = TestEnv::new();
	let runtime = tokio::runtime::Runtime::new().unwrap();
	runtime.block_on(async {
		env.init_db().await;
		env.seed_registry().await;
	});
	let registry = env.start_fake_registry(&[("latest", DIGEST_E)]);

	let stdout = env.run_ok(false, &["image", "rm", "svamm:latest"]);
	assert!(
		stdout.contains("deleted registry image svamm:latest"),
		"missing registry delete; got:\n{stdout}"
	);
	assert!(
		stdout.contains("repository svamm is now empty"),
		"must inform when the last tag is removed; got:\n{stdout}"
	);
	assert!(
		registry
			.deleted
			.lock()
			.unwrap()
			.contains(&DIGEST_E.to_string()),
		"last manifest must be deleted from the registry"
	);

	// the now-empty repository must not appear in a later listing.
	let ls = env.run_ok(false, &["image", "ls"]);
	assert!(
		ls.contains("no images in the local registry"),
		"empty repository must not be listed; got:\n{ls}"
	);
	assert!(!ls.contains("svamm"), "stale repository row; got:\n{ls}");
}

#[test]
fn image_rm_keeps_pinned_cache_copy_and_names_the_user() {
	// alpha's config pins a different digest, so the config-based guard does
	// not trigger. its rootfs is pinned to the manifest behind svamm:latest
	// (DIGEST_E). `Image::remove` then reports `ImageInUse` with a bare
	// sandbox database id.
	let mut env = TestEnv::new();
	let runtime = tokio::runtime::Runtime::new().unwrap();
	runtime.block_on(async {
		env.init_db().await;
		env.seed_registry().await;
		env.seed_sandbox(
			"alpha",
			"Running",
			Some(("svamm", "managed")),
			oci_config(env.port, "alpha", "/home/agent/proj", DIGEST_D),
		)
		.await;
	});
	let registry = env.start_fake_registry(&[("latest", DIGEST_E)]);
	let cache_ref = format!("127.0.0.1:{}/svamm:latest", env.port);
	runtime.block_on(async {
		env.seed_cached_image(&cache_ref, DIGEST_E).await;
		let db = env.db().await;
		let alpha_id: i64 = sqlx::query_scalar("SELECT id FROM sandbox WHERE name = 'alpha'")
			.fetch_one(&db)
			.await
			.unwrap();
		let manifest_id: i64 = sqlx::query_scalar("SELECT id FROM manifest WHERE digest = ?")
			.bind(DIGEST_E)
			.fetch_one(&db)
			.await
			.unwrap();
		sqlx::query(
			"INSERT INTO sandbox_rootfs (sandbox_id, manifest_id, mode, upper_fstype, created_at) \
             VALUES (?, ?, 'erofs', 'ext4', NULL)",
		)
		.bind(alpha_id)
		.bind(manifest_id)
		.execute(&db)
		.await
		.unwrap();
	});

	let stdout = env.run_ok(false, &["image", "rm", "svamm:latest"]);
	assert!(
		stdout.contains("deleted registry image svamm:latest"),
		"registry manifest must be deleted; got:\n{stdout}"
	);
	assert!(
		registry
			.deleted
			.lock()
			.unwrap()
			.contains(&DIGEST_E.to_string()),
		"unreferenced-by-config digest must be deleted from the registry"
	);
	// the cached copy is kept because alpha's rootfs is pinned to it.
	// the warning names alpha instead of its raw database id.
	assert!(
		stdout.contains("in use by alpha"),
		"warning must name the pinning sandbox; got:\n{stdout}"
	);
	assert!(
		stdout.contains("kept for now"),
		"cached copy must be kept; got:\n{stdout}"
	);
	let db = runtime.block_on(env.db());
	let remaining: Vec<String> = runtime.block_on(async {
		sqlx::query_scalar("SELECT reference FROM image_ref")
			.fetch_all(&db)
			.await
			.unwrap()
	});
	assert!(
		remaining.contains(&cache_ref),
		"pinned cached copy must remain: {remaining:?}"
	);
}

#[test]
fn image_rm_unknown_image_fails() {
	let mut env = TestEnv::new();
	let runtime = tokio::runtime::Runtime::new().unwrap();
	runtime.block_on(async {
		env.init_db().await;
		env.seed_registry().await;
	});
	env.start_fake_registry(&[("latest", common::DIGEST_A)]);

	let (stderr, _) = env.run_fail(false, &["image", "rm", "svamm:nope"]);
	assert!(
		stderr.contains("not found in the local registry"),
		"expected not-found error; stderr:\n{stderr}"
	);
}
