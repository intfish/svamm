//! tests for `svamm prune` against a fake docker-registry http server and a
//! seeded msb cache database.
//!
//! only images in use by an existing svamm sandbox are kept.

mod common;

use common::{oci_config, TestEnv, DIGEST_A, DIGEST_B, DIGEST_D, DIGEST_E, DIGEST_F};

/// an arbitrary kept tag name.
/// it survives pruning only because an existing sandbox pins its digest.
const CURRENT_TAG: &str = env!("CARGO_PKG_VERSION");

fn seed_base_sandboxes(env: &TestEnv) {
	let runtime = tokio::runtime::Runtime::new().unwrap();
	runtime.block_on(async {
		env.init_db().await;
		env.seed_registry().await;
		// alpha pins DIGEST_A. delta pins DIGEST_B.
		env.seed_sandbox(
			"alpha",
			"Running",
			Some(("svamm", "managed")),
			oci_config(env.port, "alpha", "/home/agent/proj", DIGEST_A),
		)
		.await;
		env.seed_sandbox(
			"delta",
			"Stopped",
			Some(("svamm", "managed")),
			oci_config(env.port, "delta", "/home/agent", DIGEST_B),
		)
		.await;
	});
}

#[test]
fn prune_deletes_unreferenced_images_and_keeps_referenced() {
	let mut env = TestEnv::new();
	seed_base_sandboxes(&env);

	let registry = env.start_fake_registry(&[
		(CURRENT_TAG, DIGEST_A),
		("keep-ref", DIGEST_B),
		("old-1", DIGEST_D),
		("old-2", DIGEST_E),
	]);

	let current_ref = format!("127.0.0.1:{}/svamm:{CURRENT_TAG}", env.port);
	let keep_ref = format!("127.0.0.1:{}/svamm:keep-ref", env.port);

	let runtime = tokio::runtime::Runtime::new().unwrap();
	runtime.block_on(async {
		env.seed_cached_image(&current_ref, DIGEST_A).await;
		env.seed_cached_image(&keep_ref, DIGEST_B).await;
		env.seed_cached_image(&format!("127.0.0.1:{}/svamm:old-1", env.port), DIGEST_D)
			.await;
		env.seed_cached_image(&format!("127.0.0.1:{}/svamm:old-2", env.port), DIGEST_E)
			.await;
		env.seed_cached_image("docker.io/library/alpine", DIGEST_F)
			.await;
	});

	let stdout = env.run_ok(false, &["prune"]);

	// digests pinned by existing sandboxes are kept in both stores.
	assert!(
		!stdout.contains(&current_ref),
		"image pinned by a sandbox must be kept; got:\n{stdout}"
	);
	assert!(
		!stdout.contains(&keep_ref),
		"referenced image must be kept; got:\n{stdout}"
	);
	assert!(
		!stdout.contains("alpine"),
		"non-svamm image must be kept; got:\n{stdout}"
	);

	// unreferenced old-1 and old-2 are removed from both stores.
	assert!(
		stdout.contains(&format!(
			"deleted unreferenced registry image 127.0.0.1:{}/svamm:old-1",
			env.port
		)),
		"missing registry delete for old-1; got:\n{stdout}"
	);
	assert!(
		stdout.contains(&format!(
			"deleted unreferenced registry image 127.0.0.1:{}/svamm:old-2",
			env.port
		)),
		"missing registry delete for old-2; got:\n{stdout}"
	);
	assert!(
		stdout.contains(&format!(
			"removed unreferenced cached image 127.0.0.1:{}/svamm:old-1",
			env.port
		)),
		"missing cache remove for old-1; got:\n{stdout}"
	);
	assert!(
		stdout.contains(&format!(
			"removed unreferenced cached image 127.0.0.1:{}/svamm:old-2",
			env.port
		)),
		"missing cache remove for old-2; got:\n{stdout}"
	);

	// registry deletions exactly match the unreferenced digests.
	let deleted = registry.deleted.lock().unwrap().clone();
	assert!(
		deleted.contains(&DIGEST_D.to_string()),
		"old-1 digest not deleted: {deleted:?}"
	);
	assert!(
		deleted.contains(&DIGEST_E.to_string()),
		"old-2 digest not deleted: {deleted:?}"
	);
	assert!(
		!deleted.contains(&DIGEST_A.to_string()),
		"alpha (in-use) digest wrongly deleted: {deleted:?}"
	);
	assert!(
		!deleted.contains(&DIGEST_B.to_string()),
		"delta digest wrongly deleted: {deleted:?}"
	);

	// cached image_ref rows are removed for unreferenced images only.
	let runtime = tokio::runtime::Runtime::new().unwrap();
	let db = runtime.block_on(env.db());
	let remaining: Vec<String> = runtime.block_on(async {
		sqlx::query_scalar(
			"SELECT ir.reference FROM image_ref ir JOIN manifest m ON m.id = ir.manifest_id \
             ORDER BY ir.reference",
		)
		.fetch_all(&db)
		.await
		.unwrap()
	});
	assert!(
		remaining.contains(&current_ref),
		"in-use cache ref must remain: {remaining:?}"
	);
	assert!(
		remaining.contains(&keep_ref),
		"keep-ref cache ref must remain: {remaining:?}"
	);
	assert!(
		!remaining.contains(&format!("127.0.0.1:{}/svamm:old-1", env.port)),
		"old-1 cache ref must be gone: {remaining:?}"
	);
	assert!(
		!remaining.contains(&format!("127.0.0.1:{}/svamm:old-2", env.port)),
		"old-2 cache ref must be gone: {remaining:?}"
	);
	assert!(
		remaining.contains(&"docker.io/library/alpine".to_string()),
		"alpine cache ref must remain: {remaining:?}"
	);
}

#[test]
fn prune_dry_run_previews_without_deleting() {
	let mut env = TestEnv::new();
	seed_base_sandboxes(&env);

	let registry = env.start_fake_registry(&[(CURRENT_TAG, DIGEST_A), ("old-1", DIGEST_D)]);

	let runtime = tokio::runtime::Runtime::new().unwrap();
	runtime.block_on(async {
		env.seed_cached_image(
			&format!("127.0.0.1:{}/svamm:{CURRENT_TAG}", env.port),
			DIGEST_A,
		)
		.await;
		env.seed_cached_image(&format!("127.0.0.1:{}/svamm:old-1", env.port), DIGEST_D)
			.await;
	});

	let stdout = env.run_ok(true, &["prune"]);

	assert!(
		stdout.contains(&format!(
			"would delete registry image 127.0.0.1:{}/svamm:old-1",
			env.port
		)),
		"dry-run must preview registry deletion; got:\n{stdout}"
	);
	assert!(
		stdout.contains(&format!(
			"would remove cached image 127.0.0.1:{}/svamm:old-1",
			env.port
		)),
		"dry-run must preview cache removal; got:\n{stdout}"
	);
	assert!(
		stdout.contains("would garbage-collect the local registry"),
		"dry-run must preview garbage collection; got:\n{stdout}"
	);

	// nothing was actually deleted.
	assert!(
		registry.deleted.lock().unwrap().is_empty(),
		"dry-run must not delete registry manifests"
	);
	let runtime = tokio::runtime::Runtime::new().unwrap();
	let db = runtime.block_on(env.db());
	let count: i64 = runtime.block_on(async {
		sqlx::query_scalar("SELECT COUNT(*) FROM image_ref")
			.fetch_one(&db)
			.await
			.unwrap()
	});
	assert_eq!(count, 2, "dry-run must not remove cache refs");
}

#[test]
fn prune_reports_when_nothing_to_prune() {
	let mut env = TestEnv::new();
	seed_base_sandboxes(&env);

	let registry = env.start_fake_registry(&[(CURRENT_TAG, DIGEST_A), ("keep-ref", DIGEST_B)]);

	let runtime = tokio::runtime::Runtime::new().unwrap();
	runtime.block_on(async {
		env.seed_cached_image(
			&format!("127.0.0.1:{}/svamm:{CURRENT_TAG}", env.port),
			DIGEST_A,
		)
		.await;
		env.seed_cached_image(&format!("127.0.0.1:{}/svamm:keep-ref", env.port), DIGEST_B)
			.await;
	});

	let stdout = env.run_ok(false, &["prune"]);
	assert!(
		stdout.contains("no unreferenced svamm images to prune"),
		"expected nothing-to-prune note; got:\n{stdout}"
	);
	assert!(
		!stdout.contains("image rm"),
		"no cache removals expected:\n{stdout}"
	);
	assert!(
		registry.deleted.lock().unwrap().is_empty(),
		"no registry deletions expected"
	);
}

#[test]
fn prune_removes_unused_configured_images() {
	let mut env = TestEnv::new();
	seed_base_sandboxes(&env);

	// the configured `images.vm` and the mirrored `svamm:latest` tag both
	// resolve to digests that no sandbox uses.
	let registry = env.start_fake_registry(&[("latest", DIGEST_E)]);

	let runtime = tokio::runtime::Runtime::new().unwrap();
	runtime.block_on(async {
		env.seed_cached_image("ghcr.io/intfish/svamm-images/svamm:latest", DIGEST_D)
			.await;
		env.seed_cached_image(&format!("127.0.0.1:{}/svamm:latest", env.port), DIGEST_E)
			.await;
	});

	let stdout = env.run_ok(false, &["prune"]);

	assert!(
		stdout.contains(
			"removed unreferenced cached image ghcr.io/intfish/svamm-images/svamm:latest"
		),
		"unused configured vm image must be pruned; got:\n{stdout}"
	);
	assert!(
		stdout.contains(&format!(
			"deleted unreferenced registry image 127.0.0.1:{}/svamm:latest",
			env.port
		)),
		"unused mirrored local tag must be pruned; got:\n{stdout}"
	);
	assert!(
		registry
			.deleted
			.lock()
			.unwrap()
			.contains(&DIGEST_E.to_string()),
		"unused digest must be deleted from the registry"
	);

	let runtime = tokio::runtime::Runtime::new().unwrap();
	let db = runtime.block_on(env.db());
	let remaining: Vec<String> = runtime.block_on(async {
		sqlx::query_scalar(
			"SELECT ir.reference FROM image_ref ir JOIN manifest m ON m.id = ir.manifest_id \
             ORDER BY ir.reference",
		)
		.fetch_all(&db)
		.await
		.unwrap()
	});
	assert!(
		!remaining.contains(&"ghcr.io/intfish/svamm-images/svamm:latest".to_string()),
		"unused configured vm image must be gone: {remaining:?}"
	);
}

#[test]
fn prune_deletes_shared_digest_once_and_reports_all_tags() {
	// two tags can share one manifest digest. deleting that digest removes
	// both tags. prune must delete the shared digest once, not twice.
	let mut env = TestEnv::new();
	seed_base_sandboxes(&env);
	let registry = env.start_fake_registry(&[("v1", DIGEST_D), ("v2", DIGEST_D)]);

	let stdout = env.run_ok(false, &["prune"]);

	let expected = format!(
		"deleted unreferenced registry image 127.0.0.1:{}/svamm:v1, v2",
		env.port
	);
	assert!(
		stdout.contains(&expected),
		"both shared tags must be reported in a single deletion; got:\n{stdout}"
	);
	assert!(
		!stdout.contains("failed to delete"),
		"shared digest must not be double-deleted; got:\n{stdout}"
	);
	let deleted = registry.deleted.lock().unwrap().clone();
	assert_eq!(
		deleted,
		vec![DIGEST_D.to_string()],
		"shared digest must be deleted exactly once: {deleted:?}"
	);
}
