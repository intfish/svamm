use microsandbox::Image;
use microsandbox::sandbox::{Sandbox, SandboxConfig, SandboxHandle, SandboxStatus};
use microsandbox_cli::commands::common::apply_sandbox_opts;
use microsandbox_image::{
	GlobalCache, Platform, PullOptions, Reference, Registry, RootfsMaterialization,
};

use crate::assets;
use crate::cli;
use crate::config::{self, Config, VERSION};
use crate::mounts::{
	MountSpec, abs_path, check_mount_conflicts, mount_kind, mount_spec, project_config,
	project_mount_spec, project_mounts,
};
use crate::paths;
use crate::ports;
use crate::project;
use crate::reconcile;
use crate::registry;
use crate::sandbox::{
	find_sandbox, find_sandbox_or_err, list_all, managed_selector, sandbox_oci_info,
	sandbox_running, workdir_from_handle,
};
use crate::ui;
use crate::validate::{
	require_disk_size, require_memory_size, require_quota_size, require_slug, require_tmpfs_size,
};

/// write a default `config.toml` into the config directory when absent.
/// users get an editable copy of every svamm setting.
fn write_config_file(config: &Config) -> anyhow::Result<()> {
	let path = crate::paths::config_dir().join("config.toml");
	if path.exists() {
		return Ok(());
	}
	let toml = toml::to_string_pretty(&config)
		.map_err(|err| anyhow::anyhow!("failed to serialize config: {err}"))?;
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent).ok();
	}
	std::fs::write(&path, toml)?;
	ui::note(format!("wrote default config to {}", path.display()));
	Ok(())
}

/// `svamm setup` - install the toolchain and local registry.
pub async fn setup(config: &Config) -> anyhow::Result<()> {
	config.validate()?;
	assets::ensure_default_configs(config);
	write_config_file(config)?;

	ui::step("checking host prerequisites");
	let backend = microsandbox::backend::default_backend();
	if backend.as_local().is_none() {
		anyhow::bail!("svamm requires a local microsandbox backend");
	}

	let sdk_config = microsandbox::config::load_persisted_config_or_default()
		.map_err(|err| anyhow::anyhow!("failed to read microsandbox config: {err}"))?;
	if !microsandbox::setup::is_runtime_installed(&sdk_config) {
		ui::step("installing microsandbox runtime (msb + libkrunfw)");
		microsandbox::setup::ensure_runtime(
			&sdk_config,
			microsandbox::setup::InstallOptions::default(),
		)
		.await
		.map_err(|err| anyhow::anyhow!("failed to install microsandbox runtime: {err}"))?;
	}

	ui::step("ensure local registry configuration");
	registry::ensure_registry_config(config)?;

	ui::step(format!(
		"installing registry image ({})",
		config.registry_image_ref()
	));
	registry::install_registry_image(config).await?;

	ui::step(format!(
		"creating registry sandbox ({} on 127.0.0.1:{}, proxy on 127.0.0.1:{})",
		config.registry.name, config.registry.host_port, config.registry.proxy_port
	));
	// recreate so the registry always runs the image matching this svamm version.
	registry::recreate_registry_sandbox(config).await?;

	ui::step(format!(
		"ensuring VM image ({})",
		config.sandbox_image_ref()
	));
	registry::ensure_vm_image(config).await?;

	ui::note(format!(
		"local registry (host admin): 127.0.0.1:{}",
		config.registry.host_port
	));
	ui::note(format!(
		"read-only proxy (sandbox):   127.0.0.1:{}",
		config.registry.proxy_port
	));
	ui::note(format!(
		"sandbox registry:            svamm.internal:{}",
		config.registry.proxy_port
	));
	ui::note(format!(
		"push images from host:       docker push 127.0.0.1:{}/<image>",
		config.registry.host_port
	));
	ui::note(format!(
		"pull images in sandbox:      docker pull svamm.internal:{}/<image>",
		config.registry.proxy_port
	));

	crate::completions::install()?;

	ui::step("ensuring command links (svamm, svm)");
	crate::completions::ensure_command_links()?;

	ui::ok(format!("setup complete (svamm {VERSION})"));
	Ok(())
}

/// bring up the local registry best-effort.
///
/// commands that do not strictly need the registry call this. if the registry
/// sandbox cannot start, print a warning instead of failing the command.
async fn ensure_registry_best_effort(config: &Config) {
	if let Err(err) = registry::ensure_registry(config).await {
		ui::warn(format!(
			"local registry could not be started ({err}); run `svamm setup` to repair it"
		));
	}
}

/// `svamm create` - create and boot a named sandbox.
pub async fn create(config: &Config, args: &cli::CreateCli) -> anyhow::Result<()> {
	config.validate()?;
	require_slug(&args.name).map_err(anyhow::Error::msg)?;
	let name = args.name.clone();

	let quota = match &args.quota {
		Some(value) => value.clone(),
		None => config.sandbox.workspace_quota.clone(),
	};
	require_quota_size(&quota).map_err(anyhow::Error::msg)?;

	// effective memory settings. the flag wins, then the config, then resources.yml.
	let memory = args
		.memory
		.clone()
		.or_else(|| config.sandbox.memory.clone());
	if let Some(ref memory) = memory {
		require_memory_size(memory).map_err(anyhow::Error::msg)?;
	}
	let max_memory = args
		.max_memory
		.clone()
		.or_else(|| config.sandbox.max_memory.clone());
	if let Some(ref max_memory) = max_memory {
		require_memory_size(max_memory).map_err(anyhow::Error::msg)?;
	}

	let mut specs: Vec<String> = Vec::new();
	let mut main_dest: Option<String> = None;
	if let Some(mount) = &args.mount {
		let spec = mount_spec(mount, "rw", &config.agent.home);
		main_dest = Some(spec.dest.clone());
		specs.push(format!(
			"{}:{}:{},quota={}",
			spec.source, spec.dest, spec.mode, quota
		));
	}
	for mount in &args.rw {
		let spec = mount_spec(mount, "rw", &config.agent.home);
		specs.push(format!(
			"{}:{}:{},quota={}",
			spec.source, spec.dest, spec.mode, quota
		));
	}
	for mount in &args.ro {
		let spec = mount_spec(mount, "ro", &config.agent.home);
		specs.push(format!("{}:{}:{}", spec.source, spec.dest, spec.mode));
	}
	// the create-time cli specs (main + `--rw`/`--ro`) are persisted so a
	// later `start`/`restart` can replay this exact merge from the current
	// project `svamm.json`; only project-declared mounts may change.
	let cli_specs = specs.clone();

	let project = args
		.mount
		.as_deref()
		.and_then(|mount| project_config(&abs_path(mount)));

	// merge project-declared mounts with the `--rw`/`--ro` flags.
	// destination conflicts are rejected below.
	if let Some(project) = project.as_deref()
		&& let Some(mounts) = project_mounts(project).map_err(anyhow::Error::msg)?
	{
		for mount in mounts {
			specs.push(project_mount_spec(&mount, &quota, &config.agent.home));
		}
	}
	let sandbox_image_ref = project::resolve_sandbox_image_ref(config, project.as_deref())?;
	if sandbox_image_ref != config.sandbox_image_ref() && !config::dry_run() {
		ui::note(format!("using project image {sandbox_image_ref}"));
	}

	let mut conflict_specs = vec![format!("[tmpfs]:{}:rw", config.sandbox.tmpfs_dest)];
	conflict_specs.extend(specs.iter().cloned());
	check_mount_conflicts(&conflict_specs).map_err(anyhow::Error::msg)?;

	for path in args
		.conf
		.iter()
		.chain(args.resource_conf.iter())
		.chain(args.net_conf.iter())
		.chain(args.fs_conf.iter())
	{
		if !path.is_file() {
			anyhow::bail!("missing config file: {}", path.display());
		}
	}

	let mut mount_dir: Vec<String> = Vec::new();
	let tmpfs_size = args
		.tmpfs_size
		.as_deref()
		.unwrap_or(&config.sandbox.tmpfs_size);
	require_tmpfs_size(tmpfs_size).map_err(anyhow::Error::msg)?;
	let tmpfs = format!("{}:{}", config.sandbox.tmpfs_dest, tmpfs_size);
	let mut mount_file: Vec<String> = Vec::new();
	for spec in &specs {
		let parsed = MountSpec::from_spec(spec);
		match mount_kind(&parsed.source) {
			"dir" => mount_dir.push(spec.clone()),
			"file" => mount_file.push(spec.clone()),
			_ => anyhow::bail!("mount source does not exist: {}", parsed.source),
		}
	}

	// how the project config appears inside the sandbox follows the project's
	// explicit `svamm_json` mode: masked by default, readonly on request.
	if let (Some(project_path), Some(guest_main)) = (project.as_deref(), main_dest.as_deref()) {
		let visibility =
			project::svamm_json_visibility(project_path).map_err(anyhow::Error::msg)?;
		if visibility == project::SvammJsonVisibility::Masked {
			project::ensure_mask_file()?;
		}
		let host = project::svamm_json_overlay_host(project_path, visibility);
		mount_file.push(format!("{host}:{guest_main}/svamm.json:ro"));
	}

	let merged_rules = project::merge_net_rules(config, project.as_deref())?;

	let mut port_specs: Vec<String> = Vec::new();
	if let Some(project) = project.as_deref()
		&& let Some(mappings) = ports::project_ports(project).map_err(anyhow::Error::msg)?
	{
		port_specs = mappings
			.iter()
			.map(|mapping| mapping.to_port_string())
			.collect();
	}

	let env = project::desired_env(config, project.as_deref()).map_err(anyhow::Error::msg)?;

	let mut entries = config::default_config_entries(config);
	for path in &args.conf {
		entries.push(("--conf", path.clone()));
	}
	for path in &args.resource_conf {
		entries.push(("--resource-conf", path.clone()));
	}
	for path in &args.net_conf {
		entries.push(("--net-conf", path.clone()));
	}
	for path in &args.fs_conf {
		entries.push(("--fs-conf", path.clone()));
	}

	// everything above validates. bring the registry up only now, so a failing
	// invocation does not pay for a boot it does not need.
	ensure_registry_best_effort(config).await;

	let sources = config::build_config_sources(&entries)?;
	let resolved = microsandbox_cli::sandbox_config::resolve(&sources)?;
	let builder = Sandbox::builder(name.clone());
	let builder = resolved.apply(builder)?;
	let image = resolved.image(Some(&sandbox_image_ref), None)?;
	let builder = image.apply(builder)?;

	let root_disk = if args.no_flat {
		None
	} else {
		let size = args
			.root_disk_size
			.as_deref()
			.unwrap_or(&config.sandbox.root_disk_size);
		require_disk_size(size).map_err(anyhow::Error::msg)?;
		Some(format!("flat:{size},clone=auto"))
	};

	let workdir = main_dest
		.clone()
		.unwrap_or_else(|| config.agent.home.clone());
	let opts = microsandbox_cli::commands::common::SandboxOpts {
		name: Some(name.clone()),
		label: vec![config.managed_label()],
		workdir: Some(workdir.clone()),
		env,
		cpus: args.cpus.or(config.sandbox.cpus),
		max_cpus: args.max_cpus.or(config.sandbox.max_cpus),
		memory,
		max_memory,
		mount_dir,
		mount_file,
		tmpfs: vec![tmpfs],
		root_disk,
		replace: args.replace,
		port: port_specs,
		net_rule: merged_rules.to_tokens(),
		net_default_egress: Some("deny".to_string()),
		net_strict: Some(config::network_strict(config).map_err(anyhow::Error::msg)?),
		..Default::default()
	};
	let builder = apply_sandbox_opts(builder, &opts)?;
	let builder = builder.detached(true);

	let config_out = builder.build().await?;

	if config::dry_run() {
		println!(
			"{}",
			serde_json::to_string_pretty(&config_out)
				.map_err(|err| anyhow::anyhow!("failed to serialize sandbox config: {err}"))?
		);
		return Ok(());
	}

	let (mut progress, task) = Sandbox::create_detached_with_pull_progress(config_out);
	while let Some(event) = progress.recv().await {
		ui::pull_progress(&sandbox_image_ref, &event);
	}
	match task.await {
		Ok(Ok(_sandbox)) => {
			let mut applied_guests = vec![config.sandbox.tmpfs_dest.clone()];
			for spec in &specs {
				applied_guests.push(MountSpec::from_spec(spec).dest);
			}
			if let Some(guest_main) = main_dest.as_deref() {
				applied_guests.push(format!("{guest_main}/svamm.json"));
			}
			record_mount_replay(
				&name,
				&cli_specs,
				&config.sandbox.tmpfs_dest,
				tmpfs_size,
				&quota,
				&applied_guests,
			)
			.await;
			if args.replace {
				ui::ok(format!("replaced sandbox {name}"));
			} else {
				ui::ok(format!("created sandbox {name}"));
			}
		}
		Ok(Err(err)) => return Err(anyhow::anyhow!("{err}")),
		Err(err) => return Err(anyhow::anyhow!("create task panicked: {err}")),
	}
	Ok(())
}

/// record the mount inputs create just used, so later starts can replay them.
///
/// a failure here (e.g. the config row was just written) only disables future
/// mount reconciliation for this sandbox, so it warns instead of failing the
/// already-successful create.
async fn record_mount_replay(
	name: &str,
	cli_specs: &[String],
	tmpfs_dest: &str,
	tmpfs_size: &str,
	workspace_quota: &str,
	applied_guests: &[String],
) {
	if let Err(err) = reconcile::record_mount_replay(
		name,
		cli_specs,
		tmpfs_dest,
		tmpfs_size,
		workspace_quota,
		applied_guests,
	)
	.await
	{
		ui::warn(format!(
			"could not record mount replay inputs for {name}: {err}"
		));
	}
}

/// `svamm list` - list svamm sandboxes.
pub async fn list(_config: &Config, args: &cli::ListCli) -> anyhow::Result<()> {
	let mut handles = list_all(Some(managed_selector())).await?;
	handles.sort_by(|left, right| left.name().cmp(right.name()));
	if args.quiet {
		for handle in &handles {
			println!("{}", handle.name());
		}
		return Ok(());
	}
	if handles.is_empty() {
		ui::warn_note("no svamm sandboxes");
		return Ok(());
	}
	let mut table = ui::Table::new(&["NAME", "IMAGE", "CPUS", "MEM", "STATUS"]);
	for handle in &handles {
		let config = serde_json::from_str::<SandboxConfig>(handle.config_json()).ok();
		let image = config
			.as_ref()
			.map(sandbox_image)
			.unwrap_or_else(|| "-".to_string());
		let status = format!("{:?}", handle.status_snapshot());
		let resources = resource_config(handle, config.as_ref());
		let (cpus, mem) = resources
			.as_ref()
			.map(sandbox_resources)
			.unwrap_or_else(|| ("-".to_string(), "-".to_string()));
		table.add_row(vec![
			handle.name().to_string(),
			image,
			cpus,
			mem,
			ui::format_status(&status),
		]);
	}
	table.print();
	Ok(())
}

/// the config whose allocations describe the sandbox now.
///
/// use the active-config snapshot for running sandboxes (it tracks live
/// resizes). fall back to the desired config.
fn resource_config(
	handle: &SandboxHandle,
	desired: Option<&SandboxConfig>,
) -> Option<SandboxConfig> {
	handle
		.active_config()
		.ok()
		.flatten()
		.or_else(|| desired.cloned())
}

/// the image reference for a sandbox config, or `-` when unavailable.
fn sandbox_image(config: &SandboxConfig) -> String {
	match &config.spec.image {
		microsandbox::sandbox::RootfsSource::Oci(oci) => oci.reference.clone(),
		microsandbox::sandbox::RootfsSource::Bind { path, .. } => path.display().to_string(),
		microsandbox::sandbox::RootfsSource::DiskImage { path, .. } => path.display().to_string(),
	}
}

/// render `(CPUS, MEM)` cells.
///
/// the burstable ceiling is shown only when it differs from the effective allocation.
fn sandbox_resources(config: &SandboxConfig) -> (String, String) {
	let resources = &config.spec.resources;
	let cpus = if resources.max_cpus > resources.cpus {
		format!("{} / {}", resources.cpus, resources.max_cpus)
	} else {
		resources.cpus.to_string()
	};
	let mem = if resources.max_memory_mib > resources.memory_mib {
		format!(
			"{} / {}",
			format_mem_mib(resources.memory_mib),
			format_mem_mib(resources.max_memory_mib)
		)
	} else {
		format_mem_mib(resources.memory_mib)
	};
	(cpus, mem)
}

/// format a mib memory value for the table (whole gib when exact).
fn format_mem_mib(mib: u32) -> String {
	if mib >= 1024 && mib.is_multiple_of(1024) {
		format!("{} GiB", mib / 1024)
	} else {
		format!("{mib} MiB")
	}
}

/// `svamm net` - list a sandbox's network policy and published ports.
pub async fn net(args: &cli::NetCli) -> anyhow::Result<()> {
	require_slug(&args.name).map_err(anyhow::Error::msg)?;
	let handle = find_sandbox_or_err(&args.name).await?;
	let config = handle
		.config()
		.map_err(|err| anyhow::anyhow!("failed to parse sandbox config: {err}"))?;
	let ports = ports::from_config(&config);
	let mut shown = false;
	if let Some(policy) = config.spec.network.policy.as_ref() {
		print!("{}", crate::policy::render(policy));
		shown = true;
	}
	if !ports.is_empty() {
		println!("published ports ({}):", ports.len());
		let mut table = ui::Table::new(&["HOST", "GUEST", "PROTOCOL"]);
		for mapping in &ports {
			table.add_row(vec![
				mapping.host_endpoint(),
				mapping.guest_port.to_string(),
				if mapping.udp { "udp" } else { "tcp" }.to_string(),
			]);
		}
		table.print();
		shown = true;
	}
	if !shown {
		ui::warn_note(format!("sandbox {} has no network policy", args.name));
	}
	Ok(())
}

/// `svamm mounts` - list a sandbox's mounts.
pub async fn mounts(args: &cli::MountsCli) -> anyhow::Result<()> {
	require_slug(&args.name).map_err(anyhow::Error::msg)?;
	let handle = find_sandbox_or_err(&args.name).await?;
	let config = handle
		.config()
		.map_err(|err| anyhow::anyhow!("failed to parse sandbox config: {err}"))?;
	let mounts = &config.spec.mounts;
	if mounts.is_empty() {
		ui::warn_note(format!("sandbox {} has no mounts", args.name));
		return Ok(());
	}
	println!("mounts ({}):", mounts.len());
	let mut table = ui::Table::new(&["SOURCE", "DEST", "MODE", "LIMIT"]);
	for mount in mounts {
		table.add_row(mount_cells(mount));
	}
	table.print();
	Ok(())
}

/// the table cells for a mount in `svamm mounts`.
fn mount_cells(mount: &microsandbox_types::VolumeMount) -> Vec<String> {
	match mount {
		microsandbox_types::VolumeMount::Bind {
			host,
			guest,
			options,
			quota_mib,
			..
		} => vec![
			host.display().to_string(),
			guest.clone(),
			mount_mode(options).to_string(),
			quota_mib
				.map(format_mem_mib)
				.unwrap_or_else(|| "-".to_string()),
		],
		microsandbox_types::VolumeMount::Tmpfs {
			guest,
			size_mib,
			options,
		} => vec![
			"[tmpfs]".to_string(),
			guest.clone(),
			mount_mode(options).to_string(),
			size_mib
				.map(format_mem_mib)
				.unwrap_or_else(|| "-".to_string()),
		],
		microsandbox_types::VolumeMount::Owned {
			guest,
			storage,
			options,
			..
		} => {
			let limit = match storage {
				microsandbox_types::OwnedVolumeStorage::Directory { quota_mib } => quota_mib
					.map(format_mem_mib)
					.unwrap_or_else(|| "-".to_string()),
				microsandbox_types::OwnedVolumeStorage::Disk { capacity_mib } => {
					format_mem_mib(*capacity_mib)
				}
			};
			vec![
				"[owned]".to_string(),
				guest.clone(),
				mount_mode(options).to_string(),
				limit,
			]
		}
		microsandbox_types::VolumeMount::Named {
			name,
			guest,
			create,
			options,
			..
		} => {
			let limit = create
				.as_ref()
				.and_then(|create| create.quota_mib.or(create.capacity_mib))
				.map(format_mem_mib)
				.unwrap_or_else(|| "-".to_string());
			vec![
				format!("[volume {name}]"),
				guest.clone(),
				mount_mode(options).to_string(),
				limit,
			]
		}
		microsandbox_types::VolumeMount::DiskImage {
			host,
			guest,
			options,
			..
		} => vec![
			host.display().to_string(),
			guest.clone(),
			mount_mode(options).to_string(),
			"-".to_string(),
		],
	}
}

/// the rw/ro mode of a mount.
fn mount_mode(options: &microsandbox_types::MountOptions) -> &'static str {
	if options.readonly { "ro" } else { "rw" }
}

/// `svamm start` - start a sandbox.
pub async fn start(config: &Config, args: &cli::StartCli) -> anyhow::Result<()> {
	require_slug(&args.name).map_err(anyhow::Error::msg)?;
	find_sandbox_or_err(&args.name).await?;
	reconcile::reconcile_project_ports(&args.name).await?;
	reconcile::reconcile_project_net_rules(config, &args.name).await?;
	reconcile::reconcile_network_strict(config, &args.name).await?;
	reconcile::reconcile_project_mounts(config, &args.name).await?;
	reconcile::reconcile_project_env(config, &args.name).await?;
	ensure_registry_best_effort(config).await;
	ensure_startable(config, &args.name).await?;
	if config::dry_run() {
		println!("start {}", args.name);
		return Ok(());
	}
	// detached so the sandbox survives this process exiting; an attached
	// start would watchdog-stop the vm when `svamm start` returns.
	Sandbox::start_detached(&args.name).await?;
	Ok(())
}

/// `svamm stop` - stop a sandbox (or every svamm sandbox).
pub async fn stop(args: &cli::StopCli) -> anyhow::Result<()> {
	if args.all {
		if args.name.is_some() {
			anyhow::bail!("-a cannot be combined with a sandbox name");
		}
		let handles = list_all(Some(managed_selector())).await?;
		if config::dry_run() {
			for handle in &handles {
				println!("stop {}", handle.name());
			}
			return Ok(());
		}
		for handle in &handles {
			handle.stop().await?;
		}
		return Ok(());
	}
	let Some(name) = &args.name else {
		anyhow::bail!("missing required argument: name");
	};
	require_slug(name).map_err(anyhow::Error::msg)?;
	if config::dry_run() {
		println!("stop {name}");
		return Ok(());
	}
	let Some(handle) = find_sandbox(name).await? else {
		return Ok(());
	};
	handle.stop().await?;
	Ok(())
}

/// `svamm restart` - restart a sandbox.
pub async fn restart(config: &Config, args: &cli::RestartCli) -> anyhow::Result<()> {
	require_slug(&args.name).map_err(anyhow::Error::msg)?;
	find_sandbox_or_err(&args.name).await?;
	reconcile::reconcile_project_ports(&args.name).await?;
	reconcile::reconcile_project_net_rules(config, &args.name).await?;
	reconcile::reconcile_network_strict(config, &args.name).await?;
	reconcile::reconcile_project_mounts(config, &args.name).await?;
	reconcile::reconcile_project_env(config, &args.name).await?;
	ensure_registry_best_effort(config).await;
	ensure_startable(config, &args.name).await?;
	if config::dry_run() {
		println!("restart {}", args.name);
		return Ok(());
	}
	let Some(handle) = find_sandbox(&args.name).await? else {
		return Ok(());
	};
	handle.restart().await?;
	Ok(())
}

/// `svamm rm` - stop and remove a sandbox.
pub async fn rm(args: &cli::RmCli) -> anyhow::Result<()> {
	require_slug(&args.name).map_err(anyhow::Error::msg)?;
	if config::dry_run() {
		if sandbox_running(&args.name).await? {
			println!("stop {}", args.name);
		}
		println!("rm {}", args.name);
		return Ok(());
	}
	if sandbox_running(&args.name).await?
		&& let Some(handle) = find_sandbox(&args.name).await?
	{
		handle.stop().await?;
	}
	Sandbox::remove(&args.name).await?;
	crate::state::remove(&args.name)?;
	Ok(())
}

/// `svamm run` - run a command in a sandbox.
pub async fn run(config: &Config, args: &cli::RunCli) -> anyhow::Result<()> {
	require_slug(&args.name).map_err(anyhow::Error::msg)?;
	let handle = find_sandbox_or_err(&args.name).await?;

	ensure_registry_best_effort(config).await;

	let running = handle.status_snapshot() == SandboxStatus::Running;
	if !running {
		reconcile::reconcile_project_ports(&args.name).await?;
		reconcile::reconcile_project_net_rules(config, &args.name).await?;
		reconcile::reconcile_network_strict(config, &args.name).await?;
		reconcile::reconcile_project_mounts(config, &args.name).await?;
		reconcile::reconcile_project_env(config, &args.name).await?;
	}
	if config::dry_run() {
		if !running {
			println!("start {}", args.name);
		}
		let workdir = workdir_from_handle(&handle);
		print_dry_run_exec(config, args, workdir.as_deref());
		return Ok(());
	}

	let sandbox = if running {
		handle.connect().await?
	} else {
		ensure_startable(config, &args.name).await?;
		// detached so this session can exit without terminating the vm:
		// an attached start watches this process, and its exit would stop
		// the vm (killing every other attached `run -t` session with it).
		Sandbox::start_detached(&args.name).await?
	};

	let shell = config.agent.shell.clone();
	let user_args = args.root.then(|| "root".to_string());

	let code = if args.tty {
		attach_command(&sandbox, &shell, vec![], None, user_args.as_deref()).await?
	} else if !args.command.is_empty() {
		attach_command(
			&sandbox,
			&args.command[0],
			args.command[1..].to_vec(),
			None,
			user_args.as_deref(),
		)
		.await?
	} else if args.root {
		attach_command(&sandbox, &shell, vec![], None, user_args.as_deref()).await?
	} else {
		let workdir = workdir_from_handle(&handle).unwrap_or_else(|| config.agent.home.clone());
		attach_command(&sandbox, "opencode", vec![], Some(workdir), None).await?
	};
	if code != 0 {
		std::process::exit(code);
	}
	Ok(())
}

async fn attach_command(
	sandbox: &microsandbox::sandbox::Sandbox,
	command: &str,
	args: Vec<String>,
	cwd: Option<String>,
	user: Option<&str>,
) -> anyhow::Result<i32> {
	sandbox
		.attach_with(command, |attach| {
			let attach = attach.args(args);
			let attach = match cwd {
				Some(cwd) => attach.cwd(cwd),
				None => attach,
			};
			match user {
				Some(user) => attach.user(user),
				None => attach,
			}
		})
		.await
		.map_err(|err| anyhow::anyhow!("{err}"))
}

fn print_dry_run_exec(config: &Config, args: &cli::RunCli, workdir: Option<&str>) {
	let shell = config.agent.shell.clone();
	let user = if args.root { "--user root " } else { "" };
	if args.tty {
		println!("exec -t {user}{} -- {shell}", args.name);
	} else if !args.command.is_empty() {
		println!("exec -t {user}{} -- {}", args.name, args.command.join(" "));
	} else if args.root {
		println!("exec -t {user}{} -- {shell}", args.name);
	} else {
		let wd = workdir.unwrap_or("/home/agent");
		println!("exec -t -w {wd} {} -- opencode", args.name);
	}
}

/// `svamm image` - image tooling.
pub async fn image(config: &Config, args: &cli::ImageCli) -> anyhow::Result<()> {
	config.validate()?;
	match &args.command {
		cli::ImageCommand::Pull(inner) => crate::image::pull(config, inner).await,
		cli::ImageCommand::Mirror(inner) => crate::image::mirror(config, inner).await,
		cli::ImageCommand::New(inner) => crate::image::new(config, inner),
		cli::ImageCommand::Build(inner) => crate::image::build(config, inner).await,
		cli::ImageCommand::List(inner) => crate::image::list(config, inner).await,
		cli::ImageCommand::Rm(inner) => crate::image::rm(config, inner).await,
	}
}

/// `svamm prune` - delete unreferenced svamm images.
///
/// only images in use by an existing svamm sandbox are kept.
pub async fn prune(config: &Config) -> anyhow::Result<()> {
	ensure_registry_best_effort(config).await;

	let keep_digests = registry::used_digests().await?;

	let mut pruned_registry = false;
	let mut pruned_cache = false;

	let registry_tags = registry::registry_tags(config);
	// deleting a manifest by digest removes every tag that points at it.
	// group unreferenced tags by digest so a shared digest is deleted exactly once,
	// and the shared tags are reported together.
	let mut groups: Vec<(String, String)> = Vec::new();
	let mut seen_digests: Vec<String> = Vec::new();
	for tag in registry_tags {
		let Some(tag_digest) = registry::registry_tag_digest(config, &tag) else {
			continue;
		};
		if keep_digests.contains(&tag_digest) {
			continue;
		}
		match seen_digests.iter().position(|digest| digest == &tag_digest) {
			Some(index) => {
				groups[index].0.push_str(&format!(", {tag}"));
			}
			None => {
				seen_digests.push(tag_digest.clone());
				groups.push((tag, tag_digest));
			}
		}
	}

	for (tags, tag_digest) in &groups {
		if config::dry_run() {
			println!(
				"svamm prune: would delete registry image {}:{tags} ({tag_digest})",
				config.registry_ref_prefix()
			);
		} else if registry::registry_delete_digest(config, tag_digest) {
			ui::pruned(format!(
				"deleted unreferenced registry image {}:{tags}",
				config.registry_ref_prefix()
			));
			pruned_registry = true;
		} else {
			ui::warn(format!(
				"failed to delete registry image {}:{tags}",
				config.registry_ref_prefix()
			));
		}
	}

	if !groups.is_empty() {
		if config::dry_run() {
			println!("svamm prune: would garbage-collect the local registry");
		} else if pruned_registry {
			ui::step(format!(
				"garbage-collecting the local registry ({})",
				config.registry.name
			));
			let result = registry::registry_garbage_collect(config).await;
			if result.is_err() {
				ui::warn("registry garbage-collect failed");
			}
		}
	}

	let cache_refs = registry::cached_svamm_refs(config).await?;
	let mut cache_refs_to_remove: Vec<String> = Vec::new();
	for (reference, digest) in &cache_refs {
		if !keep_digests.contains(digest) {
			cache_refs_to_remove.push(reference.clone());
		}
	}

	for reference in &cache_refs_to_remove {
		if config::dry_run() {
			println!("svamm prune: would remove cached image {reference}");
		} else if Image::remove(reference, false).await.is_ok() {
			ui::pruned(format!("removed unreferenced cached image {reference}"));
			pruned_cache = true;
		} else {
			ui::warn(format!("failed to remove cached image {reference}"));
		}
	}

	if !config::dry_run()
		&& !pruned_registry
		&& !pruned_cache
		&& groups.is_empty()
		&& cache_refs_to_remove.is_empty()
	{
		ui::ok("no unreferenced svamm images to prune");
	}
	Ok(())
}

/// materialize a missing flat-disk vmdk descriptor before starting a stopped
/// sandbox. msb start validates that the descriptor exists.
async fn ensure_startable(config: &Config, name: &str) -> anyhow::Result<()> {
	if config::dry_run() {
		return Ok(());
	}
	let Some(handle) = find_sandbox(name).await? else {
		return Ok(());
	};
	let (reference, digest, kind) = sandbox_oci_info(handle.config_json());
	if reference.is_empty() || digest.is_empty() || kind != "flat" {
		return Ok(());
	}
	let cache = GlobalCache::new(&paths::msb_home().join("cache"))?;
	let parsed_digest: microsandbox_image::Digest = digest
		.parse()
		.map_err(|err| anyhow::anyhow!("invalid manifest digest {digest}: {err}"))?;
	if cache.vmdk_path(&parsed_digest).exists() {
		return Ok(());
	}
	ui::step(format!("materializing missing disk image for {name}"));
	let mut insecure_hosts = vec![config.registry_host()];
	insecure_hosts.extend(config.insecure_image_registry_hosts());
	let registry = Registry::builder(Platform::host_linux(), cache)
		.add_insecure_registries(insecure_hosts)
		.build()
		.map_err(|err| anyhow::anyhow!("failed to build registry client: {err}"))?;
	let reference: Reference = reference
		.parse()
		.map_err(|err| anyhow::anyhow!("invalid image reference {reference}: {err}"))?;
	let options = PullOptions {
		pull_policy: microsandbox_image::PullPolicy::IfMissing,
		force: false,
		materialization: RootfsMaterialization::All,
	};
	let (mut progress, task) = registry.pull_with_progress(&reference, &options);
	while let Some(event) = progress.recv().await {
		ui::pull_progress(&reference.to_string(), &event);
	}
	task.await
		.map_err(|err| anyhow::anyhow!("pull task panicked: {err}"))?
		.map_err(|err| anyhow::anyhow!("failed to pull {reference}: {err}"))?;
	Ok(())
}
