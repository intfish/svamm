//! `svamm image` tooling.
//!
//! these commands help work with images around the local registry:
//! - fetch the public svamm vm base into the local docker daemon, so custom
//!   layers can be built `FROM svamm:<tag>`;
//! - mirror arbitrary images into the local registry so sandboxes can pull
//!   them through the read-only proxy without extra network rules;
//! - scaffold and build a custom vm image and push it to the local registry.

use std::process::Command;

use crate::config::{self, Config};
use crate::ui;

/// `svamm image pull` - fetch the configured vm base image into the local
/// docker daemon, tagged `svamm:<tag>`.
pub async fn pull(config: &Config, args: &crate::cli::ImagePullCli) -> anyhow::Result<()> {
	let reference = resolve_base(config, args.reference.as_deref())?;
	let local = format!("svamm:{}", base_tag(&reference));
	if config::dry_run() {
		println!("svamm image pull: would pull {reference} and tag it as {local}");
		return Ok(());
	}
	// the vm image may live in the local registry; docker cannot pull it
	// while the registry sandbox is down.
	if is_local_registry_reference(config, &reference) {
		crate::registry::ensure_registry(config).await?;
		crate::registry::wait_ready(config).await?;
	}
	run_docker(&["pull", &reference])?;
	run_docker(&["tag", &reference, &local])?;
	ui::ok(format!(
		"base image available as {local}; build custom layers with `FROM {local}`"
	));
	Ok(())
}

/// true when `reference` points at the svamm-managed local registry.
fn is_local_registry_reference(config: &Config, reference: &str) -> bool {
	let Ok(parsed) = reference.parse::<microsandbox_image::Reference>() else {
		return false;
	};
	parsed.registry() == config.registry_host()
}

/// `svamm image mirror` - copy an image into the local registry for sandbox use.
pub async fn mirror(config: &Config, args: &crate::cli::ImageMirrorCli) -> anyhow::Result<()> {
	if !config::dry_run() {
		crate::registry::ensure_registry(config).await?;
		crate::registry::wait_ready(config).await?;
	}
	let reference = resolve_base(config, args.reference.as_deref())?;
	let name = if args.reference.is_none() && args.as_name.is_none() {
		format!("svamm:{}", base_tag(&reference))
	} else {
		match &args.as_name {
			Some(name) if !name.trim().is_empty() => name.clone(),
			Some(_) => anyhow::bail!("--as must not be empty"),
			None => mirror_local_name(&reference)?,
		}
	};
	let local_ref = format!("{}/{name}", config.registry_host());
	run_docker(&["pull", &reference])?;
	run_docker(&["tag", &reference, &local_ref])?;
	run_docker(&["push", &local_ref])?;
	ui::ok(format!("mirrored {reference} as {local_ref}"));
	ui::note(format!(
		"pull inside a sandbox: docker pull svamm.internal:{}/{}",
		config.registry.proxy_port, name
	));
	Ok(())
}

/// `svamm image new` - scaffold a custom vm image project.
pub fn new(config: &Config, args: &crate::cli::ImageNewCli) -> anyhow::Result<()> {
	let dir = &args.dir;
	std::fs::create_dir_all(dir)
		.map_err(|err| anyhow::anyhow!("failed to create {}: {err}", dir.display()))?;
	let dockerfile = dir.join("Dockerfile");
	if dockerfile.exists() {
		anyhow::bail!(
			"{} already exists; refusing to overwrite",
			dockerfile.display()
		);
	}
	let base = format!("svamm:{}", base_tag(&resolve_base(config, None)?));
	let content = format!(
		"# Custom svamm vm image, layered on the public base image.\n\
		 # Build and push it with: svamm image build {} --name <repo>:<tag>\n\
		 # Then point images.vm (or a project svamm.json \"image\") at the printed reference.\n\
		 FROM {base}\n\
		 \n\
		 USER root\n\
		 \n\
		 # install your custom packages/tools here, e.g.\n\
		 # RUN apt-get update && apt-get install -y <packages> && rm -rf /var/lib/apt/lists/*\n\
		 \n\
		 USER agent\n\
		 WORKDIR /home/agent\n",
		dir.display()
	);
	std::fs::write(&dockerfile, content)
		.map_err(|err| anyhow::anyhow!("failed to write {}: {err}", dockerfile.display()))?;
	ui::ok(format!(
		"scaffolded {}; build it with `svamm image build {} --name <repo>:<tag>`",
		dockerfile.display(),
		dir.display()
	));
	Ok(())
}

/// `svamm image build` - build a custom vm image layer and push it to the
/// local registry, printing the reference to configure.
pub async fn build(config: &Config, args: &crate::cli::ImageBuildCli) -> anyhow::Result<()> {
	let dir = &args.dir;
	if !dir.join("Dockerfile").exists() {
		anyhow::bail!(
			"{} has no Dockerfile (scaffold one with `svamm image new {}`)",
			dir.display(),
			dir.display()
		);
	}
	let name = args.name.trim();
	if name.is_empty() || !name.contains(':') {
		anyhow::bail!("--name must be <repo>:<tag>, got '{}'", args.name);
	}
	if !config::dry_run() {
		crate::registry::ensure_registry(config).await?;
		crate::registry::wait_ready(config).await?;
	}
	let local_ref = format!("{}/{name}", config.registry_host());
	run_docker(&docker_build_args(
		&local_ref,
		dir.to_str().unwrap_or("."),
		args.no_cache,
	))?;
	run_docker(&["push", &local_ref])?;
	ui::ok(format!("built and pushed {local_ref}"));
	invalidate_cached_image(&local_ref).await;
	ui::note(format!(
		"global config: images.vm = \"{local_ref}\" in {}",
		crate::paths::config_dir().join("config.toml").display()
	));
	ui::note(format!(
		"per project:   \"image\": \"{local_ref}\" in the project svamm.json"
	));
	Ok(())
}

/// `svamm image ls` - list images stored in the local registry.
///
/// when the registry is unreachable, svamm tries to bring it up and waits for
/// it to answer. if it still does not answer, a warning with the repair hint
/// is printed instead of failing. each row carries the sandbox-facing pull
/// uri.
pub async fn list(config: &Config, _args: &crate::cli::ImageListCli) -> anyhow::Result<()> {
	let mut catalog = crate::registry::registry_catalog(config);
	if catalog.is_none() {
		let _ = crate::registry::ensure_registry(config).await;
		if !config::dry_run() {
			let _ = crate::registry::wait_ready(config).await;
		}
		catalog = crate::registry::registry_catalog(config);
	}
	let Some(mut repositories) = catalog else {
		ui::warn(format!(
			"local registry on {} is not reachable; run `svamm setup` to repair it",
			config.registry_host()
		));
		return Ok(());
	};
	repositories.sort();
	if repositories.is_empty() {
		ui::warn_note("no images in the local registry");
		return Ok(());
	}
	let mut table = ui::Table::new(&["REPOSITORY", "TAG", "SIZE", "PULL"]);
	let mut listed = 0;
	for repository in repositories {
		let mut tags = crate::registry::registry_tags_for(config, &repository);
		tags.sort();
		// a repository that lost its last manifest stays in the catalog as an
		// empty shell. it is not pull-able, so skip it.
		if tags.is_empty() {
			continue;
		}
		for tag in tags {
			let size = crate::registry::registry_manifest_size(config, &repository, &tag)
				.map(human_size)
				.unwrap_or_else(|| "-".to_string());
			let pull = format!(
				"svamm.internal:{}/{repository}:{tag}",
				config.registry.proxy_port
			);
			table.add_row(vec![repository.clone(), tag, size, pull]);
			listed += 1;
		}
	}
	if listed == 0 {
		ui::warn_note("no images in the local registry");
		return Ok(());
	}
	table.print();
	Ok(())
}

/// `svamm image rm` - remove an image from the local registry and the msb
/// cache.
///
/// nothing is removed while an existing sandbox still references the image
/// digest. the cached copy is what lets that sandbox boot. the referencing
/// sandboxes are listed so the user can decide.
pub async fn rm(config: &Config, args: &crate::cli::ImageRmCli) -> anyhow::Result<()> {
	let name = args.name.trim();
	let Some((repository, tag)) = name.rsplit_once(':') else {
		anyhow::bail!("image name must be <repo>:<tag> like `svamm:old-1`, got '{name}'");
	};
	if repository.trim().is_empty() || tag.trim().is_empty() {
		anyhow::bail!("image name must be <repo>:<tag> like `svamm:old-1`, got '{name}'");
	}
	if !config::dry_run() {
		crate::registry::ensure_registry(config).await?;
		crate::registry::wait_ready(config).await?;
	}
	let Some(digest) = crate::registry::registry_tag_digest_for(config, repository, tag) else {
		anyhow::bail!("image {name} was not found in the local registry");
	};
	let users = crate::registry::sandboxes_using_digest(&digest).await?;
	if !users.is_empty() {
		ui::warn(format!(
			"image {name} is in use by {}; nothing removed",
			users.join(", ")
		));
		return Ok(());
	}
	let cache_ref = format!("{}/{name}", config.registry_host());
	if config::dry_run() {
		println!("svamm image rm: would delete registry image {name} ({digest})");
		println!("svamm image rm: would garbage-collect the local registry");
		println!("svamm image rm: would remove cached image {cache_ref}");
		return Ok(());
	}
	if crate::registry::registry_delete_digest_for(config, repository, &digest) {
		ui::pruned(format!("deleted registry image {name}"));
		if crate::registry::registry_tags_for(config, repository).is_empty() {
			ui::note(format!(
				"repository {repository} is now empty and will not be listed"
			));
		}
	} else {
		ui::warn(format!("failed to delete registry image {name}"));
	}
	ui::step(format!(
		"garbage-collecting the local registry ({})",
		config.registry.name
	));
	if let Err(err) = crate::registry::registry_garbage_collect(config).await {
		ui::warn(format!("registry garbage-collect failed: {err}"));
	}
	remove_cached_image(&cache_ref, Some(&digest)).await;
	Ok(())
}

/// resolve the vm base image reference.
///
/// an explicit argument wins. otherwise the configured `images.vm` applies.
fn resolve_base(config: &Config, explicit: Option<&str>) -> anyhow::Result<String> {
	if let Some(reference) = explicit {
		let reference = reference.trim();
		if reference.is_empty() {
			anyhow::bail!("image reference must not be empty");
		}
		validate_reference(reference)?;
		return Ok(reference.to_string());
	}
	Ok(config.images.vm.clone())
}

/// the tag for the local `svamm` image name, derived from the reference tag.
fn base_tag(reference: &str) -> String {
	reference
		.parse::<microsandbox_image::Reference>()
		.ok()
		.and_then(|parsed| parsed.tag().map(str::to_string))
		.unwrap_or_else(|| "latest".to_string())
}

/// format a stored image size for the table (decimal units, one decimal).
fn human_size(bytes: u64) -> String {
	const KB: u64 = 1_000;
	const MB: u64 = 1_000_000;
	const GB: u64 = 1_000_000_000;
	if bytes >= GB {
		format_size(bytes, GB, "GB")
	} else if bytes >= MB {
		format_size(bytes, MB, "MB")
	} else if bytes >= KB {
		format_size(bytes, KB, "KB")
	} else {
		format!("{bytes} B")
	}
}

/// format a byte count in `unit` steps with at most one decimal, whole when exact.
fn format_size(bytes: u64, unit: u64, label: &str) -> String {
	let whole = bytes / unit;
	let first_decimal = (bytes % unit) * 10 / unit;
	if first_decimal == 0 {
		format!("{whole} {label}")
	} else {
		format!("{whole}.{first_decimal} {label}")
	}
}

/// the name to store a mirrored image under in the local registry, derived
/// from the source reference by dropping its registry host.
fn mirror_local_name(reference: &str) -> anyhow::Result<String> {
	validate_reference(reference)?;
	match reference.parse::<microsandbox_image::Reference>() {
		Ok(parsed) => {
			let tag = parsed.tag().unwrap_or("latest");
			Ok(format!("{}:{tag}", parsed.repository()))
		}
		Err(_) => Ok(reference.to_string()),
	}
}

fn validate_reference(reference: &str) -> anyhow::Result<()> {
	reference
		.parse::<microsandbox_image::Reference>()
		.map(|_| ())
		.map_err(|err| anyhow::anyhow!("invalid image reference '{reference}': {err}"))
}

/// remove the msb cache entry for a just-pushed image.
///
/// msb pulls a reference only when it is missing. keeping the cached entry
/// would boot the stale image on recreate.
async fn invalidate_cached_image(reference: &str) {
	if config::dry_run() {
		println!("svamm image build: would invalidate cached image {reference}");
		return;
	}
	match microsandbox::Image::remove(reference, false).await {
		Ok(()) => {}
		Err(microsandbox::MicrosandboxError::ImageNotFound(_)) => {}
		Err(microsandbox::MicrosandboxError::ImageInUse(_)) => {
			let names = cached_image_users(reference, None).await;
			ui::warn(format!(
				"cached image {reference} is in use by {} and could not be invalidated; \
				 recreates may reuse the old image until it is no longer in use",
				names.join(", ")
			));
		}
		Err(err) => {
			ui::warn(format!(
				"failed to invalidate cached image {reference}: {err}"
			));
		}
	}
}

/// remove a reference from the msb cache.
///
/// a note reports when there is no cached copy. a warning reports when the
/// image is still in use or removal fails. `known_digest`, when present, is
/// the manifest digest the caller already resolved for `reference`; it avoids
/// an extra cache listing when available.
async fn remove_cached_image(reference: &str, known_digest: Option<&str>) {
	match microsandbox::Image::remove(reference, false).await {
		Ok(()) => ui::pruned(format!("removed cached image {reference}")),
		Err(microsandbox::MicrosandboxError::ImageNotFound(_)) => {
			ui::note(format!("no cached copy of {reference} to remove"))
		}
		Err(microsandbox::MicrosandboxError::ImageInUse(_)) => {
			let names = cached_image_users(reference, known_digest).await;
			ui::warn(format!(
				"cached image {reference} is in use by {}; kept for now",
				names.join(", ")
			))
		}
		Err(err) => ui::warn(format!("failed to remove cached image {reference}: {err}")),
	}
}

/// the svamm sandboxes pinning a cached image, or a generic fallback.
async fn cached_image_users(reference: &str, known_digest: Option<&str>) -> Vec<String> {
	let digest = match known_digest {
		Some(digest) => digest.to_string(),
		None => match cached_manifest_digest(reference).await {
			Some(digest) => digest,
			None => return vec!["sandbox or snapshot dependency".to_string()],
		},
	};
	match crate::registry::sandboxes_using_manifest_digest(&digest).await {
		Ok(users) if !users.is_empty() => users,
		_ => vec!["sandbox or snapshot dependency".to_string()],
	}
}

/// the manifest digest of a cached image reference, when listed.
async fn cached_manifest_digest(reference: &str) -> Option<String> {
	microsandbox::Image::list()
		.await
		.ok()?
		.into_iter()
		.find(|image| image.reference() == reference)
		.and_then(|image| image.manifest_digest().map(str::to_string))
}

/// the docker build command line for a custom vm image.
///
/// `--no-cache` passes through to docker when set.
fn docker_build_args<'a>(tag: &'a str, dir: &'a str, no_cache: bool) -> Vec<&'a str> {
	let mut args = vec!["build"];
	if no_cache {
		args.push("--no-cache");
	}
	args.push("-t");
	args.push(tag);
	args.push(dir);
	args
}

/// run a docker command.
///
/// the full command line prints first. the docker process inherits the
/// terminal so the user sees its output. fail with the exit status on error.
fn run_docker(args: &[&str]) -> anyhow::Result<()> {
	let command = format!("docker {}", args.join(" "));
	ui::step(&command);
	let status = Command::new("docker")
		.args(args)
		.status()
		.map_err(|err| anyhow::anyhow!("failed to run docker: {err}"))?;
	if !status.success() {
		anyhow::bail!(
			"docker {} failed (exit {})",
			args[0],
			status.code().unwrap_or(-1)
		);
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn mirror_local_name_strips_registry_host() {
		assert_eq!(
			mirror_local_name("docker.io/library/postgres:16").unwrap(),
			"library/postgres:16"
		);
		assert_eq!(
			mirror_local_name("ghcr.io/intfish/svamm-images/svamm:1.2.3").unwrap(),
			"intfish/svamm-images/svamm:1.2.3"
		);
		assert_eq!(
			mirror_local_name("127.0.0.1:50000/acme:latest").unwrap(),
			"acme:latest"
		);
	}

	#[test]
	fn mirror_local_name_rejects_invalid_refs() {
		assert!(mirror_local_name("not a ref!!").is_err());
	}

	#[test]
	fn resolve_base_prefers_explicit_reference() {
		let config = Config::default();
		assert_eq!(
			resolve_base(&config, Some("example.com/foo:1")).unwrap(),
			"example.com/foo:1"
		);
		assert_eq!(
			resolve_base(&config, Some("  example.com/foo:1  ")).unwrap(),
			"example.com/foo:1"
		);
	}

	#[test]
	fn resolve_base_uses_configured_vm_image() {
		let mut config = Config::default();
		config.images.vm = "ghcr.io/acme/vm:9".to_string();
		assert_eq!(resolve_base(&config, None).unwrap(), "ghcr.io/acme/vm:9");
	}

	#[test]
	fn base_tag_derives_from_reference_tag() {
		assert_eq!(base_tag("ghcr.io/acme/vm:9"), "9");
		assert_eq!(base_tag("ghcr.io/acme/vm"), "latest");
		assert_eq!(
			base_tag(
				"ghcr.io/acme/vm@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
			),
			"latest"
		);
		assert_eq!(base_tag("not a ref!!"), "latest");
	}

	#[test]
	fn is_local_registry_reference_checks_the_registry_host() {
		let mut config = Config::default();
		config.registry.host_port = 50000;
		assert!(is_local_registry_reference(
			&config,
			"127.0.0.1:50000/svamm:latest"
		));
		assert!(is_local_registry_reference(
			&config,
			"127.0.0.1:50000/acme/agent:1.2.3"
		));
		assert!(!is_local_registry_reference(
			&config,
			"ghcr.io/intfish/svamm-images/svamm:latest"
		));
		assert!(!is_local_registry_reference(
			&config,
			"127.0.0.1:59999/svamm:latest"
		));
		assert!(!is_local_registry_reference(&config, "not a ref!!"));
	}

	#[test]
	fn new_scaffolds_dockerfile() {
		let dir = tempfile::tempdir().unwrap();
		let config = Config::default();
		let args = crate::cli::ImageNewCli {
			dir: dir.path().to_path_buf(),
		};
		new(&config, &args).unwrap();
		let content = std::fs::read_to_string(dir.path().join("Dockerfile")).unwrap();
		assert!(content.contains(&format!("FROM svamm:{}", base_tag(&config.images.vm))));
		assert!(new(&config, &args).is_err());
	}

	#[tokio::test]
	async fn build_rejects_missing_name_or_dockerfile() {
		let dir = tempfile::tempdir().unwrap();
		let config = Config::default();
		let missing_dockerfile = crate::cli::ImageBuildCli {
			dir: dir.path().to_path_buf(),
			name: "acme/agent:latest".to_string(),
			no_cache: false,
		};
		assert!(
			build(&config, &missing_dockerfile)
				.await
				.unwrap_err()
				.to_string()
				.contains("no Dockerfile")
		);

		std::fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
		let bad_name = crate::cli::ImageBuildCli {
			dir: dir.path().to_path_buf(),
			name: "not-a-tag".to_string(),
			no_cache: false,
		};
		assert!(
			build(&config, &bad_name)
				.await
				.unwrap_err()
				.to_string()
				.contains("--name must be <repo>:<tag>")
		);
	}

	#[test]
	fn docker_build_args_passthrough_no_cache_flag() {
		let tag = "127.0.0.1:50000/acme/agent:latest";
		assert_eq!(
			docker_build_args(tag, "images/agent", false),
			vec!["build", "-t", tag, "images/agent"]
		);
		assert_eq!(
			docker_build_args(tag, "images/agent", true),
			vec!["build", "--no-cache", "-t", tag, "images/agent"]
		);
	}

	#[test]
	fn human_size_formats_decimal_units() {
		assert_eq!(human_size(0), "0 B");
		assert_eq!(human_size(512), "512 B");
		assert_eq!(human_size(2048), "2 KB");
		assert_eq!(human_size(1_500_000), "1.5 MB");
		assert_eq!(human_size(1048576), "1 MB");
		assert_eq!(human_size(1_250_000_000), "1.2 GB");
		assert_eq!(human_size(2_147_483_648), "2.1 GB");
		assert_eq!(human_size(2_000_000_000), "2 GB");
	}
}
