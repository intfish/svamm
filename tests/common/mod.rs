//! shared test harness: hermetic environment, db seeding, and the svamm binary runner.
//!
//! the microsandbox sdk persists sandbox state to a sqlite database under
//! `MSB_HOME`. tests seed that database directly. this lets the real sdk serve
//! list/net/prune queries while vm-boot behavior runs only in dry-run mode.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;

use sqlx::SqlitePool;

/// a hermetic test environment with its own config dir, msb home, and
/// registry ports.
pub struct TestEnv {
	dir: tempfile::TempDir,
	pub port: u16,
	pub proxy_port: u16,
}

/// a running fake docker-registry http server (catalog, tags/list, HEAD, DELETE).
///
/// blobs are not served. manifest routes answer from a repo -> (tag -> digest) map.
pub struct FakeRegistry {
	pub port: u16,
	pub deleted: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
	handle: Option<std::thread::JoinHandle<()>>,
	repos: std::sync::Arc<
		std::sync::Mutex<
			std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
		>,
	>,
}

/// a running fake oci registry that serves a minimal (empty-layer) image.
pub struct FakeOciRegistry {
	pub port: u16,
	pub manifest_digest: String,
	handle: Option<std::thread::JoinHandle<()>>,
}

/// allocate a free tcp port on loopback.
pub fn free_port() -> u16 {
	let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
	listener.local_addr().unwrap().port()
}

impl TestEnv {
	/// create a fresh hermetic environment.
	pub fn new() -> Self {
		let port = free_port();
		let mut proxy_port = free_port();
		while proxy_port == port {
			proxy_port = free_port();
		}
		Self {
			dir: tempfile::tempdir().unwrap(),
			port,
			proxy_port,
		}
	}

	pub fn config_dir(&self) -> PathBuf {
		self.dir.path().join("config")
	}

	pub fn state_dir(&self) -> PathBuf {
		self.dir.path().join("state")
	}

	pub fn mask_file(&self) -> PathBuf {
		self.state_dir().join("svamm.json.mask")
	}

	/// the state file path for a sandbox.
	pub fn state_file(&self, name: &str) -> PathBuf {
		self.state_dir().join("state").join(format!("{name}.json"))
	}

	/// write the mount-replay state a create would have recorded.
	pub fn seed_mount_state(
		&self,
		name: &str,
		cli: &[String],
		tmpfs_dest: &str,
		tmpfs_size: &str,
		applied: &[String],
	) {
		let state = serde_json::json!({
			"mounts": {
				"cli": cli,
				"tmpfs_dest": tmpfs_dest,
				"tmpfs_size": tmpfs_size,
				"applied": applied,
			}
		});
		std::fs::create_dir_all(self.state_file(name).parent().unwrap()).unwrap();
		std::fs::write(self.state_file(name), serde_json::to_vec(&state).unwrap()).unwrap();
	}

	/// read the persisted state for `name`, if any.
	pub fn read_state(&self, name: &str) -> Option<serde_json::Value> {
		let path = self.state_file(name);
		let text = std::fs::read_to_string(&path).ok()?;
		serde_json::from_str(&text).ok()
	}

	pub fn msb_home(&self) -> PathBuf {
		self.dir.path().join("msb")
	}

	pub fn db_path(&self) -> PathBuf {
		self.msb_home().join("db/msb.db")
	}

	/// a `Command` configured to run the svamm binary in this environment.
	pub fn cmd(&self, dry_run: bool) -> Command {
		let mut command = Command::new(env!("CARGO_BIN_EXE_svamm"));
		command
			.env("SVAMM_CONFIG_DIR", self.config_dir())
			.env("SVAMM_STATE_DIR", self.state_dir())
			.env("MSB_HOME", self.msb_home())
			.env("SVAMM_REGISTRY_PORT", self.port.to_string())
			.env("SVAMM_REGISTRY_PROXY_PORT", self.proxy_port.to_string())
			// a dead registry endpoint would otherwise make a readiness wait
			// burn the full 30s production bound. tests never wait that long.
			.env("SVAMM_REGISTRY_READY_TIMEOUT_MS", "1000")
			.env("HOME", self.dir.path().join("home"));
		if dry_run {
			command.env("SVAMM_DRY_RUN", "1");
		}
		command
	}

	/// mark `host` as an insecure registry in the msb global config.
	///
	/// msb pulls from it over plain http. `svamm setup` writes the same config.
	pub fn mark_msb_insecure(&self, host: &str) {
		std::fs::create_dir_all(self.msb_home()).unwrap();
		let path = self.msb_home().join("config.json");
		let data = serde_json::json!({
			"registries": { "hosts": { host: { "insecure": true } } }
		});
		std::fs::write(path, serde_json::to_vec_pretty(&data).unwrap()).unwrap();
	}

	/// run svamm expecting success. returns stdout.
	pub fn run_ok(&self, dry_run: bool, args: &[&str]) -> String {
		let output = self.cmd(dry_run).args(args).output().unwrap();
		assert!(
			output.status.success(),
			"`svamm {}` failed (rc={}): stderr:\n{}",
			args.join(" "),
			output.status.code().unwrap_or(-1),
			String::from_utf8_lossy(&output.stderr)
		);
		String::from_utf8_lossy(&output.stdout).into_owned()
	}

	/// run svamm expecting failure. returns `(stderr, exit_code)`.
	pub fn run_fail(&self, dry_run: bool, args: &[&str]) -> (String, i32) {
		let output = self.cmd(dry_run).args(args).output().unwrap();
		assert!(
			!output.status.success(),
			"`svamm {}` unexpectedly succeeded",
			args.join(" ")
		);
		(
			String::from_utf8_lossy(&output.stderr).into_owned(),
			output.status.code().unwrap_or(-1),
		)
	}

	/// run `svamm create` in dry-run mode and parse the produced config JSON.
	pub fn create_config(&self, args: &[&str]) -> serde_json::Value {
		let stdout = self.run_ok(true, &[&["create"], args].concat());
		serde_json::from_str(&stdout)
			.unwrap_or_else(|err| panic!("invalid config JSON from create: {err}: {stdout}"))
	}

	/// connect to the seeded msb database.
	pub async fn db(&self) -> SqlitePool {
		SqlitePool::connect(&format!("sqlite://{}", self.db_path().display()))
			.await
			.unwrap()
	}

	/// run `svamm list` once so the sdk creates the database.
	pub async fn init_db(&self) {
		let output = self.cmd(false).arg("list").output().unwrap();
		assert!(output.status.success());
	}

	/// insert a sandbox row (and optional label) directly into the database.
	pub async fn seed_sandbox(
		&self,
		name: &str,
		status: &str,
		label: Option<(&str, &str)>,
		config_json: serde_json::Value,
	) {
		let db = self.db().await;
		let config = serde_json::to_string(&config_json).unwrap();
		sqlx::query(
			"INSERT INTO sandbox (name, config, status, created_at, updated_at, ephemeral) \
             VALUES (?, ?, ?, ?, ?, 0)",
		)
		.bind(name)
		.bind(&config)
		.bind(status)
		.bind(Option::<&str>::None)
		.bind(Option::<&str>::None)
		.execute(&db)
		.await
		.unwrap();
		if let Some((key, value)) = label {
			let id: i64 = sqlx::query_scalar("SELECT id FROM sandbox WHERE name = ?")
				.bind(name)
				.fetch_one(&db)
				.await
				.unwrap();
			sqlx::query("INSERT INTO sandbox_labels (sandbox_id, key, value) VALUES (?, ?, ?)")
				.bind(id)
				.bind(key)
				.bind(value)
				.execute(&db)
				.await
				.unwrap();
		}
	}

	/// seed the svamm-managed registry sandbox as running.
	pub async fn seed_registry(&self) {
		self.seed_sandbox(
			"svamm-registry",
			"Running",
			Some(("svamm", "registry")),
			serde_json::json!({"name": "svamm-registry", "labels": {"svamm": "registry"}}),
		)
		.await;
	}

	/// insert a cached image (manifest and image_ref rows).
	pub async fn seed_cached_image(&self, reference: &str, digest: &str) {
		let db = self.db().await;
		sqlx::query("INSERT INTO manifest (digest, media_type, created_at) VALUES (?, ?, ?)")
			.bind(digest)
			.bind("application/vnd.docker.distribution.manifest.v2+json")
			.bind(Option::<&str>::None)
			.execute(&db)
			.await
			.unwrap();
		let manifest_id: i64 = sqlx::query_scalar("SELECT id FROM manifest WHERE digest = ?")
			.bind(digest)
			.fetch_one(&db)
			.await
			.unwrap();
		sqlx::query(
			"INSERT INTO image_ref (reference, manifest_id, created_at, updated_at) \
             VALUES (?, ?, ?, ?)",
		)
		.bind(reference)
		.bind(manifest_id)
		.bind(Option::<&str>::None)
		.bind(Option::<&str>::None)
		.execute(&db)
		.await
		.unwrap();
	}

	/// write an empty vmdk file for the digest into the msb cache.
	pub async fn seed_vmdk_file(&self, digest: &str) -> PathBuf {
		let dir = self.msb_home().join("cache/vmdk");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join(format!("{}.vmdk", digest.replace(':', "_")));
		std::fs::write(&path, b"").unwrap();
		path
	}

	/// start a fake docker-registry http server.
	///
	/// `manifests` maps tag to digest in the `svamm` repository. the handle
	/// carries a `deleted` log. the server binds an ephemeral port.
	pub fn start_fake_registry(
		&mut self,
		manifests: &[(&str, &str)],
	) -> std::sync::Arc<FakeRegistry> {
		self.start_fake_catalog_registry(&[("svamm", manifests)])
	}

	/// start a fake docker-registry http server with arbitrary repositories.
	pub fn start_fake_catalog_registry(
		&mut self,
		repos: &[(&str, &[(&str, &str)])],
	) -> std::sync::Arc<FakeRegistry> {
		self.start_catalog_registry_with_budget(repos, 0)
	}

	/// start a fake docker-registry server that answers the first `fail_first`
	/// requests with 500 before serving normally, simulating a registry that
	/// is still booting.
	pub fn start_slow_catalog_registry(
		&mut self,
		repos: &[(&str, &[(&str, &str)])],
		fail_first: usize,
	) -> std::sync::Arc<FakeRegistry> {
		self.start_catalog_registry_with_budget(repos, fail_first)
	}

	fn start_catalog_registry_with_budget(
		&mut self,
		repos: &[(&str, &[(&str, &str)])],
		fail_first: usize,
	) -> std::sync::Arc<FakeRegistry> {
		let map = std::sync::Arc::new(std::sync::Mutex::new(
			repos
				.iter()
				.map(|(repo, tags)| {
					(
						repo.to_string(),
						tags.iter()
							.map(|(tag, digest)| (tag.to_string(), digest.to_string()))
							.collect::<std::collections::BTreeMap<String, String>>(),
					)
				})
				.collect::<std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>>(
				),
		));
		let deleted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
		let budget = std::sync::Arc::new(std::sync::Mutex::new(fail_first));
		let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
		let port = listener.local_addr().unwrap().port();
		let handle = {
			let map = map.clone();
			let deleted = deleted.clone();
			let budget = budget.clone();
			std::thread::spawn(move || serve_registry(listener, map, deleted, budget))
		};
		self.port = port;
		std::sync::Arc::new(FakeRegistry {
			port,
			deleted,
			handle: Some(handle),
			repos: map,
		})
	}
}

impl Default for TestEnv {
	fn default() -> Self {
		Self::new()
	}
}

fn serve_registry(
	listener: std::net::TcpListener,
	repos: std::sync::Arc<
		std::sync::Mutex<
			std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
		>,
	>,
	deleted: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
	budget: std::sync::Arc<std::sync::Mutex<usize>>,
) {
	/// the repository part of a `/v2/<repo>/<verb>/...` path.
	fn repo_of(path: &str, verb: &str) -> Option<String> {
		let rest = path.strip_prefix("/v2/")?;
		let (repo, _) = rest.rsplit_once(verb)?;
		Some(repo.to_string())
	}
	for stream in listener.incoming() {
		let Ok(mut stream) = stream else { continue };
		let Some(request) = read_request(&mut stream) else {
			continue;
		};
		// a nonzero budget simulates a registry that is not ready yet.
		let failing = {
			let mut remaining = budget.lock().unwrap();
			if *remaining > 0 {
				*remaining -= 1;
				true
			} else {
				false
			}
		};
		if failing {
			write_response(
				&mut stream,
				&HttpResponse {
					status: 500,
					headers: vec![],
					body: None,
				},
			);
			continue;
		}
		let path = request.path.clone();
		let response = match (request.method.as_str(), path.as_str()) {
			("GET", "/v2/") => HttpResponse {
				status: 200,
				headers: vec![],
				body: None,
			},
			("GET", "/v2/_catalog") => {
				let repositories: Vec<String> = repos.lock().unwrap().keys().cloned().collect();
				let body = serde_json::json!({"repositories": repositories}).to_string();
				HttpResponse {
					status: 200,
					headers: vec![("Content-Type".to_string(), "application/json".to_string())],
					body: Some(body.into_bytes()),
				}
			}
			("GET", p) if let Some(repo) = repo_of(p, "/tags/list") => {
				if repos.lock().unwrap().get(&repo).is_none() {
					HttpResponse::not_found()
				} else {
					let tags: Vec<String> = repos
						.lock()
						.unwrap()
						.get(&repo)
						.map(|tags| tags.keys().cloned().collect())
						.unwrap_or_default();
					let body = serde_json::json!({"name": repo, "tags": tags}).to_string();
					HttpResponse {
						status: 200,
						headers: vec![("Content-Type".to_string(), "application/json".to_string())],
						body: Some(body.into_bytes()),
					}
				}
			}
			("GET", p) if let Some(repo) = repo_of(p, "/manifests/") => {
				let tag = &p[p.rfind("/manifests/").unwrap() + "/manifests/".len()..];
				let known = repos
					.lock()
					.unwrap()
					.get(&repo)
					.is_some_and(|tags| tags.contains_key(tag));
				if !known {
					HttpResponse::not_found()
				} else {
					// a fixed two-kilobyte layer so `image ls` size tests have
					// a deterministic value to assert against.
					let manifest = serde_json::json!({
						"schemaVersion": 2,
						"mediaType": "application/vnd.docker.distribution.manifest.v2+json",
						"config": {
							"mediaType": "application/vnd.docker.container.image.v1+json",
							"size": 0,
							"digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
						},
						"layers": [{
							"mediaType": "application/vnd.docker.image.rootfs.diff.tar.gzip",
							"size": 2048,
							"digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
						}]
					});
					HttpResponse {
						status: 200,
						headers: vec![(
							"Content-Type".to_string(),
							"application/vnd.docker.distribution.manifest.v2+json".to_string(),
						)],
						body: Some(serde_json::to_vec(&manifest).unwrap()),
					}
				}
			}
			("HEAD", p) if let Some(repo) = repo_of(p, "/manifests/") => {
				let tag = &p[p.rfind("/manifests/").unwrap() + "/manifests/".len()..];
				match repos
					.lock()
					.unwrap()
					.get(&repo)
					.and_then(|tags| tags.get(tag))
					.cloned()
				{
					Some(digest) => HttpResponse {
						status: 200,
						headers: vec![
							("Docker-Content-Digest".to_string(), digest),
							("Content-Length".to_string(), "0".to_string()),
						],
						body: None,
					},
					None => HttpResponse::not_found(),
				}
			}
			("DELETE", p) if let Some(repo) = repo_of(p, "/manifests/") => {
				let digest = &p[p.rfind("/manifests/").unwrap() + "/manifests/".len()..];
				let mut repos = repos.lock().unwrap();
				let known = repos
					.get(&repo)
					.is_some_and(|tags| tags.values().any(|d| d == digest));
				if known {
					deleted.lock().unwrap().push(digest.to_string());
					// real registries delete by digest. this removes every
					// tag that points at it. mirror that behavior.
					if let Some(tags) = repos.get_mut(&repo) {
						tags.retain(|_, candidate| candidate != digest);
					}
					HttpResponse {
						status: 202,
						headers: vec![],
						body: None,
					}
				} else {
					HttpResponse::not_found()
				}
			}
			_ => HttpResponse::not_found(),
		};
		write_response(&mut stream, &response);
	}
}

/// start a fake oci registry serving a minimal (empty-layer) image.
/// returns the resolved manifest digest for cache assertions.
pub fn start_fake_oci_registry() -> FakeOciRegistry {
	let config = serde_json::json!({
		"architecture": "amd64",
		"os": "linux",
		"config": {},
		"rootfs": {"type": "layers", "diff_ids": []}
	});
	let config_bytes = serde_json::to_string(&config).unwrap().into_bytes();
	let config_digest = format!("sha256:{}", sha256_hex(&config_bytes));
	let manifest = serde_json::json!({
		"schemaVersion": 2,
		"mediaType": "application/vnd.docker.distribution.manifest.v2+json",
		"config": {
			"mediaType": "application/vnd.docker.container.image.v1+json",
			"digest": config_digest,
			"size": config_bytes.len(),
		},
		"layers": [],
	});
	let manifest_bytes = serde_json::to_string(&manifest).unwrap().into_bytes();
	let manifest_digest = format!("sha256:{}", sha256_hex(&manifest_bytes));
	let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
	let port = listener.local_addr().unwrap().port();
	let digest_for_thread = manifest_digest.clone();
	let handle = std::thread::spawn(move || {
		for stream in listener.incoming() {
			let Ok(mut stream) = stream else { continue };
			let Some(request) = read_request(&mut stream) else {
				continue;
			};
			let response = if request.path == "/v2/" {
				HttpResponse {
					status: 200,
					headers: vec![],
					body: None,
				}
			} else if request.path.starts_with("/v2/svamm/manifests/") {
				HttpResponse {
					status: 200,
					headers: vec![
						(
							"Content-Type".to_string(),
							"application/vnd.docker.distribution.manifest.v2+json".to_string(),
						),
						(
							"Docker-Content-Digest".to_string(),
							digest_for_thread.clone(),
						),
					],
					body: Some(manifest_bytes.clone()),
				}
			} else if request.path.starts_with("/v2/svamm/blobs/") {
				HttpResponse {
					status: 200,
					headers: vec![],
					body: Some(config_bytes.clone()),
				}
			} else {
				HttpResponse::not_found()
			};
			write_response(&mut stream, &response);
		}
	});
	FakeOciRegistry {
		port,
		manifest_digest,
		handle: Some(handle),
	}
}

struct HttpRequest {
	method: String,
	path: String,
}

struct HttpResponse {
	status: u16,
	headers: Vec<(String, String)>,
	body: Option<Vec<u8>>,
}

impl HttpResponse {
	fn not_found() -> Self {
		Self {
			status: 404,
			headers: vec![],
			body: None,
		}
	}
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<HttpRequest> {
	use std::io::Read;
	let mut buf = Vec::new();
	let mut chunk = [0u8; 2048];
	loop {
		let n = stream.read(&mut chunk).ok()?;
		if n == 0 {
			break;
		}
		buf.extend_from_slice(&chunk[..n]);
		if buf.windows(4).any(|w| w == b"\r\n\r\n") {
			break;
		}
	}
	let text = String::from_utf8_lossy(&buf);
	let mut lines = text.lines();
	let request_line = lines.next()?;
	let mut parts = request_line.split_whitespace();
	let method = parts.next()?.to_string();
	let path = parts.next()?.to_string();
	Some(HttpRequest { method, path })
}

fn write_response(stream: &mut std::net::TcpStream, response: &HttpResponse) {
	use std::io::Write;
	let reason = match response.status {
		200 => "OK",
		202 => "Accepted",
		404 => "Not Found",
		_ => "OK",
	};
	let mut head = format!("HTTP/1.1 {} {}\r\n", response.status, reason);
	for (key, value) in &response.headers {
		head.push_str(&format!("{key}: {value}\r\n"));
	}
	if let Some(body) = &response.body {
		head.push_str(&format!("Content-Length: {}\r\n", body.len()));
	}
	head.push_str("Connection: close\r\n\r\n");
	stream.write_all(head.as_bytes()).ok();
	if let Some(body) = &response.body {
		stream.write_all(body).ok();
	}
}

pub fn sha256_hex(data: &[u8]) -> String {
	use sha2::{Digest, Sha256};
	Sha256::digest(data)
		.iter()
		.map(|byte| format!("{byte:02x}"))
		.collect()
}

pub fn expect_stderr_contains(env: &TestEnv, args: &[&str], needle: &str) {
	let (stderr, _) = env.run_fail(false, args);
	assert!(
		stderr.contains(needle),
		"expected stderr to contain [{needle}]; stderr was:\n{stderr}"
	);
}

pub fn expect_stdout_contains(env: &TestEnv, args: &[&str], needle: &str) {
	let stdout = env.run_ok(false, args);
	assert!(
		stdout.contains(needle),
		"expected stdout to contain [{needle}]; stdout was:\n{stdout}"
	);
}

pub fn not_in_stdout(env: &TestEnv, args: &[&str], needle: &str) {
	let stdout = env.run_ok(false, args);
	assert!(
		!stdout.contains(needle),
		"did not expect stdout to contain [{needle}]; stdout was:\n{stdout}"
	);
}

/// the standard svamm oci image config for a seeded flat sandbox.
pub fn oci_config(port: u16, name: &str, workdir: &str, digest: &str) -> serde_json::Value {
	serde_json::json!({
		"name": name,
		"image": {
			"Oci": {
				"reference": format!("127.0.0.1:{port}/svamm:latest"),
				"root_disk": {"kind": "flat", "size_mib": 10240, "fstype": "ext4", "clone": "auto"}
			}
		},
		"manifest_digest": digest,
		"runtime": {"workdir": workdir, "shell": "/bin/sh", "scripts": {}},
		"labels": {"svamm": "managed"},
		"env": [],
		"rlimits": [],
		"mounts": [],
		"pull_policy": "IfMissing",
		"security_profile": "default",
		"deployment_profile": "single_tenant",
		"lifecycle": {"ephemeral": false, "max_duration_secs": null, "idle_timeout_secs": null}
	})
}

pub const DIGEST_A: &str =
	"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const DIGEST_B: &str =
	"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
pub const DIGEST_D: &str =
	"sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
pub const DIGEST_E: &str =
	"sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
pub const DIGEST_F: &str =
	"sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
