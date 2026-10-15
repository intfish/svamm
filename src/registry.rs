//! local image registry lifecycle and administration.
//!
//! svamm runs its own private image registry as a dedicated sandbox. this
//! module creates and boots it, configures the msb host to treat it as
//! insecure, and implements the admin calls (tag listing, digest resolution,
//! manifest deletion, garbage collection) used by `svamm setup` and
//! `svamm prune`.

use microsandbox::sandbox::Sandbox;
use microsandbox::Image;
use microsandbox_cli::commands::common::{apply_sandbox_opts, SandboxConfigSources};

use crate::assets;
use crate::config::{self, Config};
use crate::paths;
use crate::sandbox::{
	find_sandbox, is_svamm_sandbox, list_all, sandbox_exists, sandbox_oci_info, sandbox_running,
};
use crate::ui;

/// stop a sandbox and remove it, so it can be recreated with new settings.
/// `Sandbox::remove` rejects running sandboxes, so terminate first.
async fn remove_sandbox_for_recreate(name: &str) -> anyhow::Result<()> {
	if let Some(handle) = find_sandbox(name).await? {
		handle.kill().await?;
	}
	Sandbox::remove(name).await?;
	Ok(())
}

/// make sure the local image registry sandbox exists and is running.
pub async fn ensure_registry(config: &Config) -> anyhow::Result<()> {
	if config::dry_run() {
		return Ok(());
	}
	let name = &config.registry.name;
	let create_args = registry_create_builder(config)?;
	if !sandbox_exists(name).await? {
		ui::step(format!("creating local image registry sandbox ({name})"));
		create_registry_sandbox(config, create_args).await?;
		return Ok(());
	}
	if !sandbox_running(name).await? {
		ui::step(format!("starting local image registry sandbox ({name})"));
		// detached so the registry keeps running after this svamm
		// process exits (an attached start would watchdog-stop it).
		Sandbox::start_detached(name).await?;
	}
	Ok(())
}

/// wait until the local registry answers its v2 api on the host port.
///
/// the registry sandbox may report as running before the registry process has
/// finished booting. callers that push must not proceed until the registry
/// accepts requests. the wait is bounded so a broken registry fails with a
/// repair hint instead of hanging forever.
pub async fn wait_ready(config: &Config) -> anyhow::Result<()> {
	let url = format!("http://127.0.0.1:{}/v2/", config.registry.host_port);
	let agent = registry_http();
	let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
	loop {
		if agent.get(&url).call().is_ok() {
			return Ok(());
		}
		if tokio::time::Instant::now() >= deadline {
			break;
		}
		tokio::time::sleep(std::time::Duration::from_millis(500)).await;
	}
	anyhow::bail!(
		"local registry on {} did not become ready; run `svamm setup` to repair it",
		config.registry_host()
	)
}

/// recreate the registry sandbox with the combined image.
///
/// `svamm setup` uses this so the registry runs the image that matches this
/// svamm version. the registry data directory on the host persists.
pub async fn recreate_registry_sandbox(config: &Config) -> anyhow::Result<()> {
	let name = &config.registry.name;
	if remove_sandbox_for_recreate(name).await.is_ok() {
		ui::step(format!("removed existing registry sandbox ({name})"));
	}
	ui::step(format!("registry sandbox created ({name})"));
	let create_args = registry_create_builder(config)?;
	create_registry_sandbox(config, create_args).await
}

fn registry_create_builder(config: &Config) -> anyhow::Result<SandboxConfigSources> {
	let data_dir = config.registry_data_dir();
	std::fs::create_dir_all(&data_dir).map_err(|err| {
		anyhow::anyhow!(
			"failed to create registry data dir {}: {err}",
			data_dir.display()
		)
	})?;
	let resources = paths::expand_tilde(&config.configs.registry_resources);
	assets::ensure_one("registry-resources.yml", &resources);
	config::build_config_sources(&[("--resource-conf", resources)])
}

/// build the persisted config for the registry sandbox.
///
/// the registry sandbox publishes two host loopback ports: the writable
/// registry itself (host-only admin path) and the read-only nginx proxy (the
/// only endpoint sandboxes may reach). both services run inside the same
/// sandbox via the combined registry/proxy image.
pub async fn registry_sandbox_config(
	config: &Config,
	sources: SandboxConfigSources,
	image_ref: &str,
) -> anyhow::Result<microsandbox::sandbox::SandboxConfig> {
	let resolved = microsandbox_cli::sandbox_config::resolve(&sources)?;
	let builder = Sandbox::builder(config.registry.name.clone());
	let builder = resolved.apply(builder)?;
	let image = resolved.image(Some(image_ref), None)?;
	let builder = image.apply(builder)?;
	let name = config.registry.name.clone();
	let uid = nix_id("-u");
	let gid = nix_id("-g");
	let data_dir = config.registry_data_dir();
	let opts = microsandbox_cli::commands::common::SandboxOpts {
		name: Some(name),
		label: vec![config::REGISTRY_LABEL.to_string()],
		init: Some("/entrypoint.sh".to_string()),
		init_arg: vec!["/etc/docker/registry/config.yml".to_string()],
		port: vec![
			format!(
				"127.0.0.1:{}:{}",
				config.registry.host_port, config.registry.guest_port
			),
			format!("127.0.0.1:{}:5001", config.registry.proxy_port),
		],
		// stock registry:2 disables manifest deletion. svamm prune needs it.
		env: vec!["REGISTRY_STORAGE_DELETE_ENABLED=true".to_string()],
		mount_dir: vec![format!(
			"{}:/var/lib/registry:rw,uid={uid},gid={gid},quota={}",
			data_dir.display(),
			config.registry.quota
		)],
		..Default::default()
	};
	let builder = apply_sandbox_opts(builder, &opts)?;
	let builder = builder.detached(true);
	Ok(builder.build().await?)
}

/// create the registry sandbox with the configured combined image.
async fn create_registry_sandbox(
	config: &Config,
	sources: SandboxConfigSources,
) -> anyhow::Result<()> {
	create_registry_sandbox_with_image(config, sources, &config.registry_image_ref()).await
}

/// create the registry sandbox from an explicit image reference.
async fn create_registry_sandbox_with_image(
	config: &Config,
	sources: SandboxConfigSources,
	image_ref: &str,
) -> anyhow::Result<()> {
	let config_out = registry_sandbox_config(config, sources, image_ref).await?;
	let (mut progress, task) = Sandbox::create_detached_with_pull_progress(config_out);
	while let Some(event) = progress.recv().await {
		ui::pull_progress(image_ref, &event);
	}
	task.await
		.map_err(|err| anyhow::anyhow!("create task panicked: {err}"))?
		.map(|_sandbox| ())
		.map_err(|err| anyhow::anyhow!("{err}"))
}

/// resolve the numeric uid/gid of the current user via `id`.
fn nix_id(flag: &str) -> u32 {
	std::process::Command::new("id")
		.arg(flag)
		.output()
		.ok()
		.and_then(|output| String::from_utf8(output.stdout).ok())
		.and_then(|value| value.trim().parse().ok())
		.unwrap_or(0)
}

/// mark the registries svamm pulls from as insecure in the msb global config.
pub fn ensure_registry_config(config: &Config) -> anyhow::Result<()> {
	let path = config.msb_config_path();
	let mut hosts = vec![config.registry_host()];
	hosts.extend(config.insecure_image_registry_hosts());
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent).ok();
	}
	let mut data: serde_json::Value = match std::fs::read_to_string(&path) {
		Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text)
			.map_err(|err| anyhow::anyhow!("invalid JSON in {}: {err}", path.display()))?,
		_ => serde_json::json!({}),
	};
	let object = data
		.as_object_mut()
		.ok_or_else(|| anyhow::anyhow!("invalid JSON root"))?;
	let registries = object
		.entry("registries")
		.or_insert_with(|| serde_json::json!({"hosts": {}}))
		.as_object_mut()
		.ok_or_else(|| anyhow::anyhow!("invalid registries section"))?
		.clone();
	object.insert(
		"registries".to_string(),
		serde_json::Value::Object({
			let mut registries = registries;
			let mut hosts_map = registries
				.entry("hosts".to_string())
				.or_insert_with(|| serde_json::json!({}))
				.as_object_mut()
				.ok_or_else(|| anyhow::anyhow!("invalid hosts section"))?
				.clone();
			for host in hosts {
				hosts_map.insert(host, serde_json::json!({"insecure": true}));
			}
			registries.insert("hosts".to_string(), serde_json::Value::Object(hosts_map));
			registries
		}),
	);
	let mut output = serde_json::to_string_pretty(&data)?;
	output.push('\n');
	std::fs::write(&path, output)?;
	Ok(())
}

/// a shared http agent with a 5-second timeout for registry admin calls.
fn registry_http() -> ureq::Agent {
	ureq::Agent::config_builder()
		.timeout_global(Some(std::time::Duration::from_secs(5)))
		.build()
		.new_agent()
}

/// a v2 registry api url on the local registry for an arbitrary repository.
fn registry_v2_url_for(config: &Config, repository: &str, suffix: &str) -> String {
	format!(
		"http://127.0.0.1:{}/v2/{repository}/{suffix}",
		config.registry.host_port
	)
}

/// fetch the repository catalog from the local registry via the v2 api.
///
/// `None` when the registry is unreachable. `Some(list)` (possibly empty)
/// when the v2 api answered.
pub fn registry_catalog(config: &Config) -> Option<Vec<String>> {
	let url = format!("http://127.0.0.1:{}/v2/_catalog", config.registry.host_port);
	let agent = registry_http();
	let Ok(response) = agent.get(&url).call() else {
		return None;
	};
	let data: serde_json::Value = response.into_body().read_json().unwrap_or_default();
	let repositories = data
		.get("repositories")
		.and_then(|repositories| repositories.as_array())
		.map(|repositories| {
			repositories
				.iter()
				.filter_map(|repo| repo.as_str().map(str::to_string))
				.collect()
		})
		.unwrap_or_default();
	Some(repositories)
}

/// list tags of a repository in the local registry.
pub fn registry_tags_for(config: &Config, repository: &str) -> Vec<String> {
	let url = format!(
		"http://127.0.0.1:{}/v2/{repository}/tags/list",
		config.registry.host_port
	);
	let agent = registry_http();
	let Ok(response) = agent.get(&url).call() else {
		return Vec::new();
	};
	let data: serde_json::Value = response.into_body().read_json().unwrap_or_default();
	data.get("tags")
		.and_then(|tags| tags.as_array())
		.map(|tags| {
			tags.iter()
				.filter_map(|tag| tag.as_str().map(str::to_string))
				.collect()
		})
		.unwrap_or_default()
}

/// list tags of the `svamm` repository in the local registry.
pub fn registry_tags(config: &Config) -> Vec<String> {
	registry_tags_for(config, &config.registry.repo)
}

/// resolve a tag to its manifest digest in the `svamm` repository.
pub fn registry_tag_digest(config: &Config, tag: &str) -> Option<String> {
	registry_tag_digest_for(config, &config.registry.repo, tag)
}

/// resolve a tag to its manifest digest in a repository of the local registry.
pub fn registry_tag_digest_for(config: &Config, repository: &str, tag: &str) -> Option<String> {
	let url = registry_v2_url_for(config, repository, &format!("manifests/{tag}"));
	let agent = registry_http();
	let Ok(response) = agent
		.head(&url)
		.header(
			"Accept",
			"application/vnd.docker.distribution.manifest.v2+json",
		)
		.call()
	else {
		return None;
	};
	response
		.headers()
		.get("Docker-Content-Digest")
		.and_then(|value| value.to_str().ok())
		.map(str::to_string)
}

/// delete a manifest by digest from the `svamm` repository.
pub fn registry_delete_digest(config: &Config, digest: &str) -> bool {
	registry_delete_digest_for(config, &config.registry.repo, digest)
}

/// delete a manifest by digest from a repository of the local registry.
pub fn registry_delete_digest_for(config: &Config, repository: &str, digest: &str) -> bool {
	let url = registry_v2_url_for(config, repository, &format!("manifests/{digest}"));
	let agent = registry_http();
	agent.delete(&url).call().is_ok()
}

/// run registry garbage collection inside the registry sandbox.
pub async fn registry_garbage_collect(config: &Config) -> anyhow::Result<()> {
	let Some(handle) = find_sandbox(&config.registry.name).await? else {
		return Ok(());
	};
	let sandbox = handle.connect().await?;
	sandbox
		.exec_with("/bin/registry", |exec| {
			exec.args(["garbage-collect", "/etc/docker/registry/config.yml"])
		})
		.await?;
	Ok(())
}

/// manifest digests in use by existing svamm sandboxes.
///
/// includes the registry sandbox. used to decide what `prune`/`image rm` may remove.
pub async fn used_digests() -> anyhow::Result<Vec<String>> {
	let mut digests = Vec::new();
	let handles = list_all(None).await?;
	for handle in handles {
		if !is_svamm_sandbox(handle.config_json()) {
			continue;
		}
		let (_, digest, _) = sandbox_oci_info(handle.config_json());
		if !digest.is_empty() {
			digests.push(digest);
		}
	}
	Ok(digests)
}

/// names of existing svamm sandboxes whose rootfs manifest digest matches
/// `digest`, sorted for display.
pub async fn sandboxes_using_digest(digest: &str) -> anyhow::Result<Vec<String>> {
	let mut users = Vec::new();
	let handles = list_all(None).await?;
	for handle in handles {
		if !is_svamm_sandbox(handle.config_json()) {
			continue;
		}
		let (_, used, _) = sandbox_oci_info(handle.config_json());
		if used == digest {
			users.push(handle.name().to_string());
		}
	}
	users.sort();
	Ok(users)
}

/// names of svamm sandboxes whose rootfs is pinned to `manifest_digest` in the
/// msb database, sorted for display.
pub async fn sandboxes_using_manifest_digest(manifest_digest: &str) -> anyhow::Result<Vec<String>> {
	let db_path = paths::msb_home().join("db").join("msb.db");
	let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", db_path.display())).await?;
	let names: Vec<String> = sqlx::query_scalar(
		"SELECT s.name FROM sandbox s \
		 JOIN sandbox_rootfs r ON r.sandbox_id = s.id \
		 JOIN manifest m ON m.id = r.manifest_id \
		 WHERE m.digest = ? \
		 ORDER BY s.name COLLATE NOCASE",
	)
	.bind(manifest_digest)
	.fetch_all(&pool)
	.await
	.map_err(|err| anyhow::anyhow!("failed to query sandboxes using {manifest_digest}: {err}"))?;
	Ok(names)
}

/// the cached-image reference prefixes svamm manages.
///
/// this is the local registry repo plus the repositories of the configured
/// vm/registry images.
pub fn managed_ref_prefixes(config: &Config) -> Vec<String> {
	let mut prefixes = vec![format!("{}:", config.registry_ref_prefix())];
	for reference in [&config.images.vm, &config.images.registry] {
		if let Ok(parsed) = reference.parse::<microsandbox_image::Reference>() {
			let prefix = format!("{}/{}:", parsed.registry(), parsed.repository());
			if !prefixes.contains(&prefix) {
				prefixes.push(prefix);
			}
		}
	}
	prefixes
}

/// cached svamm image references and digests from the msb host cache.
pub async fn cached_svamm_refs(config: &Config) -> anyhow::Result<Vec<(String, String)>> {
	let prefixes = managed_ref_prefixes(config);
	let images = Image::list().await?;
	let mut refs = Vec::new();
	for image in images {
		let reference = image.reference().to_string();
		if let Some(digest) = image.manifest_digest() {
			if prefixes.iter().any(|prefix| reference.starts_with(prefix)) {
				refs.push((reference, digest.to_string()));
			}
		}
	}
	Ok(refs)
}

/// pull an image into the msb cache and persist its reference.
///
/// a sandbox can then boot from it without contacting the source registry.
async fn pull_image_into_cache(config: &Config, reference: &str) -> anyhow::Result<()> {
	let cache = microsandbox_image::GlobalCache::new(&paths::msb_home().join("cache"))?;
	let image_ref: microsandbox_image::Reference = reference
		.parse()
		.map_err(|err| anyhow::anyhow!("invalid image reference {reference}: {err}"))?;
	let options = microsandbox_image::PullOptions {
		pull_policy: microsandbox_image::PullPolicy::IfMissing,
		force: false,
		materialization: microsandbox_image::RootfsMaterialization::All,
	};
	let mut insecure_hosts = vec![config.registry_host()];
	insecure_hosts.extend(config.insecure_image_registry_hosts());
	let client = microsandbox_image::Registry::builder(
		microsandbox_image::Platform::host_linux(),
		cache.clone(),
	)
	.add_insecure_registries(insecure_hosts)
	.build()
	.map_err(|err| anyhow::anyhow!("failed to build registry client: {err}"))?;
	let (mut progress, task) = client.pull_with_progress(&image_ref, &options);
	while let Some(event) = progress.recv().await {
		ui::pull_progress(reference, &event);
	}
	task.await
		.map_err(|err| anyhow::anyhow!("pull task panicked: {err}"))?
		.map_err(|err| anyhow::anyhow!("failed to pull {reference}: {err}"))?;
	let metadata = microsandbox_image::Registry::pull_cached(&cache, &image_ref, &options)?
		.map(|(_, metadata)| metadata)
		.ok_or_else(|| anyhow::anyhow!("pulled image {reference} is not cached"))?;
	let backend = microsandbox::backend::default_backend();
	let local = backend
		.as_local()
		.ok_or_else(|| anyhow::anyhow!("svamm requires a local microsandbox backend"))?;
	microsandbox::Image::persist(local, reference, metadata)
		.await
		.map_err(|err| anyhow::anyhow!("failed to persist image reference {reference}: {err}"))?;
	Ok(())
}

/// make sure the vm image sandboxes boot from is available.
///
/// prefetch is best-effort. `svamm create` pulls on demand anyway.
pub async fn ensure_vm_image(config: &Config) -> anyhow::Result<()> {
	pull_image_into_cache(config, &config.images.vm).await
}

/// install the registry image into the msb cache.
///
/// the registry sandbox can then boot without any pre-existing registry.
pub async fn install_registry_image(config: &Config) -> anyhow::Result<()> {
	pull_image_into_cache(config, &config.registry_image_ref()).await
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn registry_sandbox_config_publishes_both_ports() {
		let dir = tempfile::tempdir().unwrap();
		let mut config = Config::default();
		config.registry.host_port = 50000;
		config.registry.proxy_port = 50001;
		config.registry.data_dir = dir.path().join("data").display().to_string();
		config.configs.registry_resources = dir
			.path()
			.join("registry-resources.yml")
			.display()
			.to_string();
		assets::ensure_one(
			"registry-resources.yml",
			std::path::Path::new(&config.configs.registry_resources),
		);

		let image_ref = config.registry_image_ref();
		let sources = registry_create_builder(&config).unwrap();
		let config_out = registry_sandbox_config(&config, sources, &image_ref)
			.await
			.unwrap();
		let value = serde_json::to_value(&config_out).unwrap();

		assert_eq!(
			value["image"]["Oci"]["reference"],
			serde_json::json!(image_ref)
		);
		assert_eq!(value["labels"]["svamm"], serde_json::json!("registry"));

		// the registry sandbox publishes the writable registry (host admin)
		// and the read-only proxy (sandbox-facing).
		let ports = value["network"]["ports"].as_array().unwrap();
		let port_strings: Vec<String> = ports
			.iter()
			.map(|p| {
				format!(
					"{}:{}/{}",
					p["host_port"].as_u64().unwrap_or(0),
					p["guest_port"].as_u64().unwrap_or(0),
					p["protocol"].as_str().unwrap_or("")
				)
			})
			.collect();
		assert!(port_strings.contains(&"50000:5000/tcp".to_string()));
		assert!(port_strings.contains(&"50001:5001/tcp".to_string()));

		// deletion must be enabled for `svamm prune`.
		let envs: Vec<String> = value["env"]
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
		assert!(envs.contains(&"REGISTRY_STORAGE_DELETE_ENABLED=true".to_string()));

		// pid 1 is the combined entrypoint running registry + proxy.
		assert_eq!(value["init"]["cmd"], serde_json::json!("/entrypoint.sh"));
	}

	#[tokio::test]
	async fn registry_sandbox_config_mounts_persisted_data_dir() {
		let dir = tempfile::tempdir().unwrap();
		let data = dir.path().join("data");
		let mut config = Config::default();
		config.registry.host_port = 50000;
		config.registry.proxy_port = 50001;
		config.registry.data_dir = data.display().to_string();
		config.configs.registry_resources = dir
			.path()
			.join("registry-resources.yml")
			.display()
			.to_string();
		assets::ensure_one(
			"registry-resources.yml",
			std::path::Path::new(&config.configs.registry_resources),
		);

		let sources = registry_create_builder(&config).unwrap();
		let config_out = registry_sandbox_config(&config, sources, &config.registry_image_ref())
			.await
			.unwrap();
		let value = serde_json::to_value(&config_out).unwrap();

		let mounts = value["mounts"].as_array().unwrap();
		let registry_mount = mounts.iter().find(|m| {
			m["guest"] == serde_json::json!("/var/lib/registry")
				&& m.get("options").and_then(|o| o.get("readonly"))
					== Some(&serde_json::json!(false))
		});
		let registry_mount = registry_mount.expect("registry data dir must be mounted rw");
		assert_eq!(
			registry_mount["quota_mib"],
			serde_json::json!(32 * 1024),
			"registry data mount must carry the configured guest-write quota"
		);
	}
}
