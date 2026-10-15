//! reconciliation of project-declared settings onto a sandbox's persisted config.
//!
//! project `svamm.json` overrides (ports, net rules, strict hostname policy,
//! mounts, envs) apply to an existing sandbox by rewriting its persisted
//! config through the typed [`SandboxConfig`], without recreating the vm.
//! svamm-owned replay state lives in the [`state`] outside the msb config,
//! because msb rewrites its config from the typed struct and would drop
//! unknown fields.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use microsandbox::sandbox::SandboxConfig;
use microsandbox_types::{EnvVar, HostPermissions, MountOptions, StatVirtualization, VolumeMount};

use crate::config::{self, Config};
use crate::mounts::{
	check_mount_conflicts, mount_kind, parse_spec, project_config, project_mount_spec,
	project_mounts,
};
use crate::paths;
use crate::ports;
use crate::project;
use crate::sandbox::find_sandbox;
use crate::state::{self, MountReplay};
use crate::ui;

/// locate the project `svamm.json` for a sandbox from its persisted config.
///
/// the main rw mount is the one whose guest destination equals the sandbox
/// workdir. the project config is `<host>/svamm.json` for that mount.
fn project_config_from_sandbox(config: &SandboxConfig) -> Option<PathBuf> {
	let workdir = config.spec.runtime.workdir.as_deref()?;
	if workdir.is_empty() {
		return None;
	}
	for mount in &config.spec.mounts {
		let VolumeMount::Bind { host, guest, .. } = mount else {
			continue;
		};
		if guest == workdir {
			return project_config(host.to_str()?);
		}
	}
	None
}

/// reconcile a project's `ports` onto an existing sandbox's persisted config.
///
/// changed ports apply on the next boot without recreating the vm. returns
/// `true` when the durable config was (or, in dry-run mode, would be) updated.
pub async fn reconcile_project_ports(name: &str) -> anyhow::Result<bool> {
	let Some(handle) = find_sandbox(name).await? else {
		return Ok(false);
	};
	let mut sandbox = handle
		.config()
		.map_err(|err| anyhow::anyhow!("failed to parse sandbox config for {name}: {err}"))?;
	let Some(project) = project_config_from_sandbox(&sandbox) else {
		return Ok(false);
	};
	let Some(mappings) = ports::project_ports(&project).map_err(anyhow::Error::msg)? else {
		return Ok(false);
	};

	let current: Vec<ports::PortMapping> = sandbox
		.spec
		.network
		.ports
		.iter()
		.map(ports::from_spec)
		.collect();
	if current == mappings {
		return Ok(false);
	}

	let rendered: Vec<String> = mappings.iter().map(|m| m.to_port_string()).collect();
	let summary = if rendered.is_empty() {
		"none".to_string()
	} else {
		rendered.join(" ")
	};
	if config::dry_run() {
		println!("apply ports {name}: {summary}");
		return Ok(true);
	}

	sandbox.spec.network.enabled = true;
	sandbox.spec.network.ports = mappings.iter().map(ports::PortMapping::to_spec).collect();
	update_sandbox_config(name, &sandbox).await?;
	ui::note(format!("updated published ports for {name}: {summary}"));
	Ok(true)
}

/// reconcile a project's `network.allow`/`network.deny` onto an existing
/// sandbox's persisted config.
///
/// changed net rules apply on the next boot without recreating the vm. the
/// policy is rebuilt from the same sources `create` uses.
pub async fn reconcile_project_net_rules(config: &Config, name: &str) -> anyhow::Result<bool> {
	use microsandbox_cli::net_rule::parse_rule_token;

	let Some(handle) = find_sandbox(name).await? else {
		return Ok(false);
	};
	let mut sandbox = handle
		.config()
		.map_err(|err| anyhow::anyhow!("failed to parse sandbox config for {name}: {err}"))?;
	let Some(project) = project_config_from_sandbox(&sandbox) else {
		return Ok(false);
	};
	let merged = project::merge_net_rules(config, Some(&project))?;

	let mut rules: Vec<microsandbox_types::Rule> = Vec::new();
	for token in merged.to_tokens() {
		let rule = parse_rule_token(&token).map_err(anyhow::Error::msg)?;
		let json = serde_json::to_value(&rule)
			.map_err(|err| anyhow::anyhow!("failed to serialize net rule {token}: {err}"))?;
		let rule: microsandbox_types::Rule = serde_json::from_value(json)
			.map_err(|err| anyhow::anyhow!("failed to map net rule {token}: {err}"))?;
		rules.push(rule);
	}
	let desired = microsandbox_types::NetworkPolicy {
		default_egress: microsandbox_types::Action::Deny,
		default_ingress: microsandbox_types::Action::Allow,
		rules,
	};

	let current =
		sandbox
			.spec
			.network
			.policy
			.clone()
			.unwrap_or_else(|| microsandbox_types::NetworkPolicy {
				default_egress: microsandbox_types::Action::Deny,
				default_ingress: microsandbox_types::Action::Deny,
				rules: vec![],
			});
	if current == desired {
		return Ok(false);
	}

	let diff_lines = net_rule_diff(&desired.rules, &current.rules);
	if config::dry_run() {
		println!("apply net-rules {name}:");
		for line in &diff_lines {
			println!("  {line}");
		}
		return Ok(true);
	}

	sandbox.spec.network.enabled = true;
	sandbox.spec.network.policy = Some(desired);
	update_sandbox_config(name, &sandbox).await?;
	ui::note(format!("updated network policy for {name}"));
	for line in &diff_lines {
		println!("  {line}");
	}
	Ok(true)
}

/// align a sandbox's persisted `network.strict` with the effective strictness.
pub async fn reconcile_network_strict(config: &Config, name: &str) -> anyhow::Result<bool> {
	if config::dry_run() {
		return Ok(false);
	}
	let Some(handle) = find_sandbox(name).await? else {
		return Ok(false);
	};
	let desired = config::network_strict(config).map_err(anyhow::Error::msg)?;

	let mut sandbox = handle
		.config()
		.map_err(|err| anyhow::anyhow!("failed to parse sandbox config for {name}: {err}"))?;
	if sandbox.spec.network.strict == desired {
		return Ok(false);
	}

	sandbox.spec.network.strict = desired;
	update_sandbox_config(name, &sandbox).await?;
	ui::note(format!(
		"updated network strict hostname policy for {name}: {}",
		if desired { "on" } else { "off" }
	));
	Ok(true)
}

/// reconcile a project's `envs` onto an existing sandbox's persisted config.
///
/// changed env vars apply on the next boot without recreating the vm. keys
/// svamm applies are recorded in the sandbox's [`SandboxState`]; env it does not
/// manage (base/image entries like `PATH`, `LANG`, `TERM`) keeps its value, and
/// a missing `envs` object drops every previously applied key.
///
/// returns `true` when the durable config was (or, in dry-run mode, would be) updated.
pub async fn reconcile_project_env(config: &Config, name: &str) -> anyhow::Result<bool> {
	let Some(handle) = find_sandbox(name).await? else {
		return Ok(false);
	};
	let mut sandbox = handle
		.config()
		.map_err(|err| anyhow::anyhow!("failed to parse sandbox config for {name}: {err}"))?;
	let Some(project) = project_config_from_sandbox(&sandbox) else {
		return Ok(false);
	};

	let mut state = state::load(name)?.unwrap_or_default();
	let current = current_env_entries(&sandbox);
	let previously_managed = state.env_managed.iter().cloned().collect();
	let project_envs = project::project_envs(&project).map_err(anyhow::Error::msg)?;
	let (env, sensitive, managed): (Vec<String>, BTreeSet<String>, BTreeSet<String>) =
		match &project_envs {
			Some(envs) => {
				let desired =
					project::desired_env_entries(config, envs).map_err(anyhow::Error::msg)?;
				// keys whose project value is a host reference resolve to
				// values that may be secrets; the reconcile diff hides them.
				let sensitive: BTreeSet<String> = envs
					.iter()
					.filter(|(_, value)| project::is_host_env_reference(value.as_str()))
					.map(|(key, _)| key.clone())
					.collect();
				// the proxy var is always regenerated, so it is never a managed key.
				let managed: BTreeSet<String> = envs
					.keys()
					.filter(|key| key.as_str() != project::REGISTRY_PROXY_ENV)
					.cloned()
					.collect();
				(
					merged_env(&current, &previously_managed, &desired),
					sensitive,
					managed,
				)
			}
			None => {
				let cleaned = drop_managed_env_keys(&current, &previously_managed);
				(cleaned, BTreeSet::new(), BTreeSet::new())
			}
		};

	let metadata_synced = project_envs.is_none()
		|| state
			.env_managed
			.iter()
			.cloned()
			.collect::<BTreeSet<String>>()
			== managed;
	if env == current && metadata_synced {
		return Ok(false);
	}

	let diff_lines = env_diff(&env, &current, &sensitive);
	if config::dry_run() {
		println!("apply env {name}:");
		for line in &diff_lines {
			println!("  {line}");
		}
		return Ok(true);
	}

	sandbox.spec.env = entries_to_env(&env);
	update_sandbox_config(name, &sandbox).await?;
	state.env_managed = managed.into_iter().collect();
	state::save(name, &state)?;
	ui::note(format!("updated environment for {name}"));
	for line in &diff_lines {
		println!("  {line}");
	}
	Ok(true)
}

/// the env list after full management of a declared `envs` object: persisted
/// entries svamm does not manage keep their values in place,
/// previously-managed keys that are no longer desired are dropped, and the
/// desired entries apply last. every key appears once.
fn merged_env(
	current: &[String],
	previously_managed: &BTreeSet<String>,
	desired: &[String],
) -> Vec<String> {
	let desired_keys: BTreeSet<&str> = desired.iter().map(|entry| env_key(entry)).collect();
	let mut env: Vec<String> = current
		.iter()
		.filter(|entry| {
			let key = env_key(entry);
			!previously_managed.contains(key) && !desired_keys.contains(key)
		})
		.cloned()
		.collect();
	env.extend(desired.iter().cloned());
	env
}

/// the env list when no `envs` object is declared: drop project keys svamm
/// previously applied, and leave everything else (including the generated
/// proxy var) byte-for-byte untouched.
fn drop_managed_env_keys(current: &[String], previously_managed: &BTreeSet<String>) -> Vec<String> {
	current
		.iter()
		.filter(|entry| {
			let key = env_key(entry);
			key == project::REGISTRY_PROXY_ENV || !previously_managed.contains(key)
		})
		.cloned()
		.collect()
}

/// the variable name of a `KEY=VALUE` entry (the whole entry when it has no `=`).
fn env_key(entry: &str) -> &str {
	entry.split_once('=').map(|(key, _)| key).unwrap_or(entry)
}

/// the persisted `env` list as `KEY=VALUE` strings from the typed spec.
fn current_env_entries(config: &SandboxConfig) -> Vec<String> {
	config
		.spec
		.env
		.iter()
		.map(|entry| format!("{}={}", entry.key, entry.value))
		.collect()
}

/// the durable `env` list: `EnvVar` objects for `KEY=VALUE` entries.
fn entries_to_env(entries: &[String]) -> Vec<EnvVar> {
	entries
		.iter()
		.map(|entry| {
			let (key, value) = entry
				.split_once('=')
				.expect("env entries are always KEY=VALUE");
			EnvVar {
				key: key.to_string(),
				value: value.to_string(),
			}
		})
		.collect()
}

/// persist the mount inputs `create` just used, so later starts can replay
/// them. `applied` lists the guest paths the sandbox's mounts now own.
pub async fn record_mount_replay(
	name: &str,
	cli_specs: &[String],
	tmpfs_dest: &str,
	tmpfs_size: &str,
	workspace_quota: &str,
	applied_guests: &[String],
) -> anyhow::Result<()> {
	let mut state = state::load(name)?.unwrap_or_default();
	state.mounts = Some(MountReplay {
		cli: cli_specs.to_vec(),
		tmpfs_dest: tmpfs_dest.to_string(),
		tmpfs_size: tmpfs_size.to_string(),
		workspace_quota: workspace_quota.to_string(),
		applied: applied_guests.to_vec(),
	});
	state::save(name, &state)
}

/// reconcile a project's `svamm.json` `mounts` onto an existing sandbox's
/// persisted config.
///
/// changed mounts apply on the next boot without recreating the vm. the
/// desired set is the exact `create` merge replayed from the recorded cli
/// specs plus the current project mounts.
///
/// returns `true` when the durable config was (or, in dry-run mode, would be) updated.
/// a sandbox without recorded replay inputs is left untouched.
pub async fn reconcile_project_mounts(config: &Config, name: &str) -> anyhow::Result<bool> {
	let Some(handle) = find_sandbox(name).await? else {
		return Ok(false);
	};
	let mut sandbox = handle
		.config()
		.map_err(|err| anyhow::anyhow!("failed to parse sandbox config for {name}: {err}"))?;
	let Some(project) = project_config_from_sandbox(&sandbox) else {
		return Ok(false);
	};
	let workdir = sandbox.spec.runtime.workdir.clone().unwrap_or_default();
	if workdir.is_empty() {
		return Ok(false);
	}

	let mut state = state::load(name)?.unwrap_or_default();
	let Some(replay) = state.mounts.clone() else {
		// every sandbox created by an svamm with mount replay records the
		// inputs at create; a missing recording is left untouched.
		return Ok(false);
	};
	if replay.cli.is_empty() {
		return Ok(false);
	}
	let visibility = project::svamm_json_visibility(&project).map_err(anyhow::Error::msg)?;

	let desired = render_mounts(config, &replay, &project, &workdir, visibility)?;
	let persisted = &sandbox.spec.mounts;

	// drop previously-applied svamm-owned guests (removed project mounts are
	// not part of the replay), keep anything svamm never owned, then apply the
	// freshly replayed set (desired wins on a guest collision).
	let applied: BTreeSet<&str> = replay.applied.iter().map(String::as_str).collect();
	let mut new_mounts: Vec<VolumeMount> = Vec::new();
	for mount in persisted {
		let owned = mount_guest(mount).is_some_and(|guest| applied.contains(guest));
		if !owned {
			new_mounts.push(mount.clone());
		}
	}
	for mount in &desired {
		let guest = mount_guest(mount);
		if let Some(guest) = guest {
			new_mounts.retain(|existing| mount_guest(existing) != Some(guest));
		}
		new_mounts.push(mount.clone());
	}

	// a masked overlay must leave the mask present: the persisted overlay
	// references it, and a missing host would be reclassified at boot.
	if !config::dry_run() && visibility == project::SvammJsonVisibility::Masked {
		project::ensure_mask_file()?;
	}
	let changed = !mounts_equivalent(&new_mounts, persisted);
	if !changed {
		return Ok(false);
	}
	let diff_lines = if changed {
		mount_diff_lines(persisted, &desired, &applied)
	} else {
		Vec::new()
	};
	if config::dry_run() {
		println!("apply mounts {name}:");
		for line in &diff_lines {
			println!("  {line}");
		}
		return Ok(true);
	}

	let desired_guests: Vec<String> = desired
		.iter()
		.filter_map(mount_guest)
		.map(str::to_string)
		.collect();
	sandbox.spec.mounts = new_mounts;
	update_sandbox_config(name, &sandbox).await?;
	if let Some(mounts) = state.mounts.as_mut() {
		mounts.applied = desired_guests;
	}
	state::save(name, &state)?;
	ui::note(format!("updated mounts for {name}"));
	for line in &diff_lines {
		println!("  {line}");
	}
	Ok(true)
}

/// the persistent `mounts` entries `create` would produce today: the recorded
/// tmpfs and cli specs, the current project mounts, and the svamm.json
/// overlay (masked or readonly per the project mode). validates duplicates and
/// source paths exactly like `create`.
fn render_mounts(
	config: &Config,
	replay: &MountReplay,
	project: &Path,
	workdir: &str,
	visibility: project::SvammJsonVisibility,
) -> anyhow::Result<Vec<VolumeMount>> {
	let mut specs: Vec<String> = replay.cli.clone();
	// state written before the quota was recorded has an empty value; fall
	// back to the configured default there.
	let quota = if replay.workspace_quota.is_empty() {
		&config.sandbox.workspace_quota
	} else {
		&replay.workspace_quota
	};
	if let Some(mounts) = project_mounts(project).map_err(anyhow::Error::msg)? {
		for mount in mounts {
			specs.push(project_mount_spec(&mount, quota, &config.agent.home));
		}
	}

	let mut conflict_specs = vec![format!("[tmpfs]:{}:rw", replay.tmpfs_dest)];
	conflict_specs.extend(specs.iter().cloned());
	check_mount_conflicts(&conflict_specs).map_err(anyhow::Error::msg)?;
	for spec in &specs {
		let (source, ..) = parse_spec(spec);
		if mount_kind(source) == "missing" {
			anyhow::bail!("mount source does not exist: {source}");
		}
	}

	let mut entries: Vec<VolumeMount> = Vec::new();
	entries.push(VolumeMount::Tmpfs {
		guest: replay.tmpfs_dest.clone(),
		size_mib: size_to_mib(&replay.tmpfs_size),
		options: MountOptions::default(),
	});
	for spec in &specs {
		let (source, guest, mode, quota) = parse_spec_parts(spec);
		let readonly = mode == "ro";
		let quota_mib = if readonly {
			None
		} else {
			quota.and_then(|quota| size_to_mib(&quota))
		};
		entries.push(VolumeMount::Bind {
			host: PathBuf::from(source),
			guest: guest.to_string(),
			options: MountOptions {
				readonly,
				..Default::default()
			},
			stat_virtualization: StatVirtualization::Strict,
			host_permissions: HostPermissions::Private,
			follow_root_symlinks: false,
			quota_mib,
		});
	}
	entries.push(VolumeMount::Bind {
		host: PathBuf::from(project::svamm_json_overlay_host(project, visibility)),
		guest: format!("{workdir}/svamm.json"),
		options: MountOptions {
			readonly: true,
			..Default::default()
		},
		stat_virtualization: StatVirtualization::Strict,
		host_permissions: HostPermissions::Private,
		follow_root_symlinks: false,
		quota_mib: None,
	});
	Ok(entries)
}

/// the guest path of a mount, when it has one.
fn mount_guest(mount: &VolumeMount) -> Option<&str> {
	match mount {
		VolumeMount::Bind { guest, .. }
		| VolumeMount::Owned { guest, .. }
		| VolumeMount::Named { guest, .. }
		| VolumeMount::DiskImage { guest, .. }
		| VolumeMount::Tmpfs { guest, .. } => Some(guest.as_str()),
	}
}

/// a one-line description of a persisted mount entry for diff output.
fn mount_entry_text(mount: &VolumeMount) -> String {
	match mount {
		VolumeMount::Bind {
			host,
			guest,
			options,
			..
		} => format!(
			"{}:{guest}:{}",
			host.display(),
			if options.readonly { "ro" } else { "rw" }
		),
		VolumeMount::Tmpfs { guest, .. } => format!("[tmpfs]:{guest}"),
		other => format!("{other:?}"),
	}
}

/// the svamm-owned mounts that are removed and added relative to the
/// persisted list, as `- ...` and `+ ...` lines (removals first, sorted).
fn mount_diff_lines(
	persisted: &[VolumeMount],
	desired: &[VolumeMount],
	owned: &BTreeSet<&str>,
) -> Vec<String> {
	let desired_guests: BTreeSet<&str> = desired.iter().filter_map(mount_guest).collect();
	let mut removed: Vec<String> = persisted
		.iter()
		.filter(|mount| {
			mount_guest(mount)
				.is_some_and(|guest| owned.contains(guest) && !desired_guests.contains(guest))
		})
		.map(|mount| format!("- {}", mount_entry_text(mount)))
		.collect();
	let persisted_guests: BTreeSet<&str> = persisted.iter().filter_map(mount_guest).collect();
	let mut added: Vec<String> = desired
		.iter()
		.filter(|mount| mount_guest(mount).is_some_and(|guest| !persisted_guests.contains(guest)))
		.map(|mount| format!("+ {}", mount_entry_text(mount)))
		.collect();
	removed.sort();
	added.sort();
	removed.extend(added);
	removed
}

/// the env entries removed and added relative to the persisted list, as
/// `- KEY=VALUE` and `+ KEY=VALUE` lines (removals first). the values of keys
/// that resolve from a host reference are hidden (`- KEY` / `+ KEY`): the
/// resolved value may be a secret.
fn env_diff(desired: &[String], current: &[String], sensitive: &BTreeSet<String>) -> Vec<String> {
	let mut lines: Vec<String> = Vec::new();
	for entry in current {
		if desired.contains(entry) {
			continue;
		}
		let key = env_key(entry);
		if sensitive.contains(key) {
			lines.push(format!("- {key}"));
		} else {
			lines.push(format!("- {entry}"));
		}
	}
	for entry in desired {
		if current.contains(entry) {
			continue;
		}
		let key = env_key(entry);
		if sensitive.contains(key) {
			lines.push(format!("+ {key}"));
		} else {
			lines.push(format!("+ {entry}"));
		}
	}
	lines
}

/// the policy rules that are removed and added relative to the persisted
/// policy, as `- ...` and `+ ...` tokens (removals first, in rule order).
fn net_rule_diff(
	desired: &[microsandbox_types::Rule],
	current: &[microsandbox_types::Rule],
) -> Vec<String> {
	let mut lines: Vec<String> = current
		.iter()
		.filter(|rule| !desired.contains(rule))
		.map(|rule| format!("- {}", rule_token(rule)))
		.collect();
	lines.extend(
		desired
			.iter()
			.filter(|rule| !current.contains(rule))
			.map(|rule| format!("+ {}", rule_token(rule))),
	);
	lines
}

/// a best-effort token rendering of a policy rule, mirroring the forms
/// `parse_rule_token` accepts. falls back to the debug form for exotic,
/// hand-edited rules so the diff always shows something readable.
fn rule_token(rule: &microsandbox_types::Rule) -> String {
	use microsandbox_types::{Action, Destination, DestinationGroup, Direction, Protocol};

	let action = match rule.action {
		Action::Allow => "allow",
		Action::Deny => "deny",
	};
	let prefix = match rule.direction {
		Direction::Egress => format!("{action}@"),
		Direction::Ingress => format!("{action}:ingress@"),
		Direction::Any => format!("{action}:any@"),
	};
	let destination = match &rule.destination {
		Destination::Any => "any".to_string(),
		Destination::Cidr(network) => network.to_string(),
		Destination::Domain(domain) => domain.clone(),
		Destination::DomainSuffix(suffix) => format!("*.{suffix}"),
		Destination::Group(group) => match group {
			DestinationGroup::Public => "public".to_string(),
			DestinationGroup::Loopback => "loopback".to_string(),
			DestinationGroup::Private => "private".to_string(),
			DestinationGroup::LinkLocal => "link-local".to_string(),
			DestinationGroup::Metadata => "meta".to_string(),
			DestinationGroup::Multicast => "multicast".to_string(),
			DestinationGroup::Host => "host".to_string(),
		},
	};
	let mut token = format!("{prefix}{destination}");
	if !rule.protocols.is_empty() {
		let protocols: Vec<&str> = rule
			.protocols
			.iter()
			.map(|protocol| match protocol {
				Protocol::Tcp => "tcp",
				Protocol::Udp => "udp",
				Protocol::Icmpv4 => "icmpv4",
				Protocol::Icmpv6 => "icmpv6",
			})
			.collect();
		token.push(':');
		token.push_str(&protocols.join(","));
	}
	if !rule.ports.is_empty() {
		let ports: Vec<String> = rule
			.ports
			.iter()
			.map(|range| {
				if range.start == range.end {
					range.start.to_string()
				} else {
					format!("{}-{}", range.start, range.end)
				}
			})
			.collect();
		token.push(':');
		token.push_str(&ports.join(","));
	}
	token
}

/// split a recorded spec into `(source, guest, mode, quota)`.
///
/// rw specs carry `,quota=<size>` on the mode field; ro specs do not.
fn parse_spec_parts(spec: &str) -> (&str, &str, &str, Option<String>) {
	let (source, dest, mode) = parse_spec(spec);
	let (mode, quota) = match mode.find(",quota=") {
		Some(index) => (
			&mode[..index],
			Some(mode[index + ",quota=".len()..].to_string()),
		),
		None => (mode, None),
	};
	(source, dest, mode, quota)
}

/// the miB value of a `N[KMGTP]` size (`16G`, `2048`, ...), or `None` when
/// malformed. the same grammar `validate` accepts for creating sandboxes.
fn size_to_mib(size: &str) -> Option<u32> {
	let size = size.trim();
	let split = size.find(|char: char| !char.is_ascii_digit());
	let (digits, unit) = match split {
		Some(index) => (&size[..index], size[index..].to_ascii_uppercase()),
		None => (size, String::new()),
	};
	let value: u64 = digits.parse().ok()?;
	let mib = match unit.as_str() {
		"" => value,
		"K" => value / 1024,
		"M" => value,
		"G" => value.saturating_mul(1024),
		"T" => value.saturating_mul(1024 * 1024),
		"P" => value.saturating_mul(1024 * 1024 * 1024),
		_ => return None,
	};
	u32::try_from(mib).ok()
}

/// semantic equality of two mount lists: keyed by guest with a normalized
/// signature per entry, ignoring key order and sdk-default fields that never
/// change for svamm-owned mounts.
fn mounts_equivalent(left: &[VolumeMount], right: &[VolumeMount]) -> bool {
	fn snapshot(mounts: &[VolumeMount]) -> BTreeMap<String, String> {
		mounts
			.iter()
			.enumerate()
			.map(|(index, mount)| {
				let guest = mount_guest(mount).unwrap_or("");
				let key = if guest.is_empty() {
					format!("#{index}")
				} else {
					guest.to_string()
				};
				(key, mount_signature(mount))
			})
			.collect()
	}
	snapshot(left) == snapshot(right)
}

/// a stable signature for one mount entry for comparison purposes.
fn mount_signature(mount: &VolumeMount) -> String {
	match mount {
		VolumeMount::Bind {
			host,
			guest,
			options,
			quota_mib,
			..
		} => format!(
			"Bind|{}|{guest}|{}|{}",
			host.display(),
			options.readonly,
			quota_mib.unwrap_or(0)
		),
		VolumeMount::Tmpfs {
			guest, size_mib, ..
		} => format!("Tmpfs|{guest}|{}", size_mib.unwrap_or(0)),
		// unknown shapes are not svamm-generated; compare their raw form.
		other => format!("{other:?}"),
	}
}

/// persist a sandbox's typed config in the msb database.
///
/// the runtime reads the durable config at every boot, so updating it before
/// the next start applies the reconciled settings. `active_config` is cleared
/// as well.
pub async fn update_sandbox_config(name: &str, config: &SandboxConfig) -> anyhow::Result<()> {
	let config_json = serde_json::to_string(config).map_err(|err| {
		anyhow::anyhow!("failed to serialize updated sandbox config for {name}: {err}")
	})?;
	let db_path = paths::msb_home().join("db").join("msb.db");
	let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", db_path.display())).await?;
	sqlx::query(
		"UPDATE sandbox SET config = ?, active_config = NULL, updated_at = CURRENT_TIMESTAMP \
		 WHERE name = ?",
	)
	.bind(config_json)
	.bind(name)
	.execute(&pool)
	.await
	.map_err(|err| anyhow::anyhow!("failed to update sandbox config for {name}: {err}"))?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn write_json(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
		let path = dir.join(name);
		std::fs::write(&path, content).unwrap();
		path
	}

	fn bind_mount(
		host: impl AsRef<std::path::Path>,
		guest: &str,
		readonly: bool,
		quota_mib: Option<u32>,
	) -> VolumeMount {
		VolumeMount::Bind {
			host: host.as_ref().to_path_buf(),
			guest: guest.to_string(),
			options: MountOptions {
				readonly,
				..Default::default()
			},
			stat_virtualization: StatVirtualization::Strict,
			host_permissions: HostPermissions::Private,
			follow_root_symlinks: false,
			quota_mib,
		}
	}

	#[test]
	fn size_to_mib_parses_sizes() {
		assert_eq!(size_to_mib("2G"), Some(2048));
		assert_eq!(size_to_mib("32G"), Some(32768));
		assert_eq!(size_to_mib("2048M"), Some(2048));
		assert_eq!(size_to_mib("1024"), Some(1024));
		assert_eq!(size_to_mib("8g"), Some(8192));
		assert_eq!(size_to_mib("1T"), Some(1048576));
		assert_eq!(size_to_mib("banana"), None);
		assert_eq!(size_to_mib("16GB"), None);
		assert_eq!(size_to_mib(""), None);
	}

	#[test]
	fn parse_spec_parts_splits_quota() {
		assert_eq!(
			parse_spec_parts("/a:/home/agent/a:rw,quota=32G"),
			("/a", "/home/agent/a", "rw", Some("32G".to_string()))
		);
		assert_eq!(
			parse_spec_parts("/b:/home/agent/b:ro"),
			("/b", "/home/agent/b", "ro", None)
		);
	}

	#[test]
	fn bind_mount_serializes_the_persisted_shape() {
		// the typed mount must serialize to the exact shape the sdk persists.
		let mount = bind_mount("/srv/ref", "/home/agent/ref", true, None);
		assert_eq!(
			serde_json::to_value(&mount).unwrap(),
			serde_json::json!({
				"type": "Bind",
				"host": "/srv/ref",
				"guest": "/home/agent/ref",
				"options": {"readonly": true, "noexec": false, "nosuid": false, "nodev": false},
				"stat_virtualization": "strict",
				"host_permissions": "private",
				"follow_root_symlinks": false,
				"quota_mib": null,
			})
		);
		let tmpfs = VolumeMount::Tmpfs {
			guest: "/tmp".to_string(),
			size_mib: Some(2048),
			options: MountOptions::default(),
		};
		let value = serde_json::to_value(&tmpfs).unwrap();
		assert_eq!(value["type"], "Tmpfs");
		assert_eq!(value["size_mib"], 2048);
	}

	#[test]
	fn mounts_equivalent_ignores_key_order() {
		let a = bind_mount("/h", "/g", true, None);
		let changed = bind_mount("/other", "/g", true, None);
		assert!(mounts_equivalent(
			std::slice::from_ref(&a),
			std::slice::from_ref(&a)
		));
		assert!(!mounts_equivalent(&[a], &[changed]));
	}

	fn seed_sandbox_config(project: &Path, workdir: &str) -> SandboxConfig {
		let value = serde_json::json!({
			"name": "alpha",
			"runtime": {"workdir": workdir},
			"mounts": [{
				"type": "Bind", "host": project.display().to_string(), "guest": workdir,
				"options": {"readonly": false, "noexec": false, "nosuid": false, "nodev": false},
				"stat_virtualization": "strict", "host_permissions": "private",
				"follow_root_symlinks": false, "quota_mib": 32768
			}],
		});
		serde_json::from_value(value).unwrap()
	}

	/// a sandbox whose only bind mounts a guest path other than its workdir.
	fn foreign_sandbox_config(project: &Path, workdir: &str, guest: &str) -> SandboxConfig {
		let value = serde_json::json!({
			"name": "alpha",
			"runtime": {"workdir": workdir},
			"mounts": [{
				"type": "Bind", "host": project.display().to_string(), "guest": guest,
				"options": {"readonly": false, "noexec": false, "nosuid": false, "nodev": false},
				"stat_virtualization": "strict", "host_permissions": "private",
				"follow_root_symlinks": false, "quota_mib": 32768
			}],
		});
		serde_json::from_value(value).unwrap()
	}

	#[test]
	fn project_config_from_sandbox_finds_the_project_file() {
		let dir = tempfile::tempdir().unwrap();
		let host = dir.path().join("host");
		std::fs::create_dir_all(&host).unwrap();
		let project = write_json(&host, "svamm.json", "{}");
		let config = seed_sandbox_config(&host, "/home/agent/proj");
		assert_eq!(project_config_from_sandbox(&config), Some(project.clone()));
		// a foreign bind at another path never resolves.
		let orphan = foreign_sandbox_config(&host, "/home/agent/other", "/elsewhere");
		assert!(project_config_from_sandbox(&orphan).is_none());
	}

	#[test]
	fn render_mounts_replays_project_mounts() {
		let dir = tempfile::tempdir().unwrap();
		let work = dir.path().join("work");
		std::fs::create_dir_all(&work).unwrap();
		let refdir = dir.path().join("ref");
		std::fs::create_dir_all(&refdir).unwrap();
		let project = write_json(
			dir.path(),
			"svamm.json",
			&format!(
				r#"{{"mounts": [{{"src": "{}", "dest": "/home/agent/ref", "mode": "ro"}}]}}"#,
				refdir.display()
			),
		);
		let config = Config::default();
		let replay = MountReplay {
			cli: vec![format!("{}:/home/agent/work:rw,quota=32G", work.display())],
			tmpfs_dest: "/tmp".to_string(),
			tmpfs_size: "2G".to_string(),
			workspace_quota: "32G".to_string(),
			applied: vec!["/tmp".to_string(), "/home/agent/work".to_string()],
		};
		let entries = render_mounts(
			&config,
			&replay,
			&project,
			"/home/agent/work",
			project::SvammJsonVisibility::Masked,
		)
		.unwrap();
		let guests: Vec<String> = entries
			.iter()
			.filter_map(mount_guest)
			.map(str::to_string)
			.collect();
		assert!(guests.contains(&"/tmp".to_string()));
		assert!(guests.contains(&"/home/agent/work".to_string()));
		assert!(guests.contains(&"/home/agent/ref".to_string()));
		assert!(
			guests.contains(&"/home/agent/work/svamm.json".to_string()),
			"overlay must be replayed: {guests:?}"
		);
		// the masked overlay reads back from the mask host, never the real file.
		let overlay = entries
			.iter()
			.find(|entry| mount_guest(entry) == Some("/home/agent/work/svamm.json"))
			.expect("overlay entry");
		let VolumeMount::Bind { host, .. } = overlay else {
			panic!("overlay must be a bind mount");
		};
		assert_eq!(host, &project::mask_file());
		// the replayed ref entry reads back through the diff renderer.
		let owned: BTreeSet<&str> = replay.applied.iter().map(String::as_str).collect();
		let diff = mount_diff_lines(&[], &entries, &owned);
		assert!(
			diff.iter().any(|line| line.contains("/home/agent/ref:ro")),
			"diff must carry the replayed mount: {diff:?}"
		);
	}

	#[test]
	fn render_mounts_fails_on_missing_source() {
		let config = Config::default();
		let dir = tempfile::tempdir().unwrap();
		let project = write_json(
			dir.path(),
			"svamm.json",
			r#"{"mounts": [{"src": "/definitely/missing", "mode": "ro"}]}"#,
		);
		let replay = MountReplay {
			cli: vec![],
			tmpfs_dest: "/tmp".to_string(),
			tmpfs_size: "2G".to_string(),
			workspace_quota: "32G".to_string(),
			applied: vec![],
		};
		let err = render_mounts(
			&config,
			&replay,
			&project,
			"/home/agent",
			project::SvammJsonVisibility::Masked,
		)
		.unwrap_err();
		assert!(
			err.to_string().contains("mount source does not exist"),
			"unexpected error: {err}"
		);
	}

	#[test]
	fn render_mounts_readonly_overlay_binds_the_real_file() {
		let dir = tempfile::tempdir().unwrap();
		let work = dir.path().join("work");
		std::fs::create_dir_all(&work).unwrap();
		let project = write_json(dir.path(), "svamm.json", "{}");
		let config = Config::default();
		let replay = MountReplay {
			cli: vec![format!("{}:/home/agent/work:rw,quota=32G", work.display())],
			tmpfs_dest: "/tmp".to_string(),
			tmpfs_size: "2G".to_string(),
			workspace_quota: "32G".to_string(),
			applied: vec!["/tmp".to_string(), "/home/agent/work".to_string()],
		};
		let entries = render_mounts(
			&config,
			&replay,
			&project,
			"/home/agent/work",
			project::SvammJsonVisibility::Readonly,
		)
		.unwrap();
		let overlay = entries
			.iter()
			.find(|entry| mount_guest(entry) == Some("/home/agent/work/svamm.json"))
			.expect("overlay entry");
		let VolumeMount::Bind { host, options, .. } = overlay else {
			panic!("overlay must be a bind mount");
		};
		assert_eq!(host, &project, "readonly mode must bind the real config");
		assert!(options.readonly);
	}

	/// the quota of a bound `guest` in a rendered mount set.
	fn rendered_quota_mib(entries: &[VolumeMount], guest: &str) -> Option<u32> {
		entries
			.iter()
			.find(|entry| mount_guest(entry) == Some(guest))
			.and_then(|entry| match entry {
				VolumeMount::Bind { quota_mib, .. } => *quota_mib,
				_ => None,
			})
	}

	#[test]
	fn render_mounts_uses_recorded_quota_for_project_mounts() {
		let dir = tempfile::tempdir().unwrap();
		let work = dir.path().join("work");
		std::fs::create_dir_all(&work).unwrap();
		let extra = dir.path().join("extra");
		std::fs::create_dir_all(&extra).unwrap();
		let project = write_json(
			dir.path(),
			"svamm.json",
			&format!(
				r#"{{"mounts": [{{"src": "{}", "dest": "/home/agent/extra", "mode": "rw"}}]}}"#,
				extra.display()
			),
		);
		let config = Config::default();
		let replay = MountReplay {
			cli: vec![format!("{}:/home/agent/work:rw,quota=32G", work.display())],
			tmpfs_dest: "/tmp".to_string(),
			tmpfs_size: "2G".to_string(),
			workspace_quota: "64G".to_string(),
			applied: vec!["/tmp".to_string(), "/home/agent/work".to_string()],
		};
		let entries = render_mounts(
			&config,
			&replay,
			&project,
			"/home/agent/work",
			project::SvammJsonVisibility::Masked,
		)
		.unwrap();
		assert_eq!(
			rendered_quota_mib(&entries, "/home/agent/extra"),
			Some(64 * 1024),
			"recorded create-time quota must apply to project mounts"
		);
	}

	#[test]
	fn render_mounts_falls_back_to_config_quota_when_unrecorded() {
		// state files written before the quota was recorded have an empty
		// value; they must behave as before (configured default).
		let dir = tempfile::tempdir().unwrap();
		let work = dir.path().join("work");
		std::fs::create_dir_all(&work).unwrap();
		let extra = dir.path().join("extra");
		std::fs::create_dir_all(&extra).unwrap();
		let project = write_json(
			dir.path(),
			"svamm.json",
			&format!(
				r#"{{"mounts": [{{"src": "{}", "dest": "/home/agent/extra", "mode": "rw"}}]}}"#,
				extra.display()
			),
		);
		let config = Config::default();
		let replay = MountReplay {
			cli: vec![format!("{}:/home/agent/work:rw,quota=32G", work.display())],
			tmpfs_dest: "/tmp".to_string(),
			tmpfs_size: "2G".to_string(),
			workspace_quota: String::new(),
			applied: vec!["/tmp".to_string(), "/home/agent/work".to_string()],
		};
		let entries = render_mounts(
			&config,
			&replay,
			&project,
			"/home/agent/work",
			project::SvammJsonVisibility::Masked,
		)
		.unwrap();
		assert_eq!(
			rendered_quota_mib(&entries, "/home/agent/extra"),
			Some(32 * 1024),
			"unrecorded quota must fall back to the configured default"
		);
	}

	/// parse a net-rule token into the persisted `Rule` shape, following the
	/// same network-rule -> policy-rule mapping the reconcile uses.
	fn parse_rule(token: &str) -> microsandbox_types::Rule {
		use microsandbox_cli::net_rule::parse_rule_token;
		let rule = parse_rule_token(token).unwrap();
		let json = serde_json::to_value(&rule).unwrap();
		serde_json::from_value::<microsandbox_types::Rule>(json).unwrap()
	}

	#[test]
	fn rule_token_round_trips_svamm_rule_forms() {
		// the rule forms svamm produces render back to their token form, so
		// the diff shows the exact entries the user wrote.
		assert_eq!(
			rule_token(&parse_rule("allow@host:tcp:5432")),
			"allow@host:tcp:5432"
		);
		assert_eq!(
			rule_token(&parse_rule("deny@svamm-project.test:tcp:443")),
			"deny@svamm-project.test:tcp:443"
		);
		assert_eq!(rule_token(&parse_rule("deny:any@host")), "deny:any@host");
		assert_eq!(rule_token(&parse_rule("allow@public")), "allow@public");
	}

	#[test]
	fn env_diff_reports_added_removed_and_changed() {
		let current: Vec<String> = vec![
			"SVAMM_REGISTRY_PROXY_PORT=50001".to_string(),
			"OLD=gone".to_string(),
			"A=old-value".to_string(),
		];
		let desired: Vec<String> = vec![
			"SVAMM_REGISTRY_PROXY_PORT=50001".to_string(),
			"A=new-value".to_string(),
			"NEW=added".to_string(),
		];
		let diff = env_diff(&desired, &current, &managed_set(&[]));
		assert!(
			diff.contains(&"- OLD=gone".to_string()),
			"removed entry must be reported: {diff:?}"
		);
		assert!(
			diff.contains(&"- A=old-value".to_string()),
			"changed value must report the old entry: {diff:?}"
		);
		assert!(
			diff.contains(&"+ A=new-value".to_string()),
			"changed value must report the new entry: {diff:?}"
		);
		assert!(
			diff.contains(&"+ NEW=added".to_string()),
			"added entry must be reported: {diff:?}"
		);
		assert!(
			!diff
				.iter()
				.any(|line| line.contains("SVAMM_REGISTRY_PROXY_PORT")),
			"unchanged entry must not appear: {diff:?}"
		);
	}

	#[test]
	fn env_diff_empty_desired_reports_all_removed() {
		let current: Vec<String> = vec!["A=1".to_string(), "B=2".to_string()];
		let diff = env_diff(&[], &current, &managed_set(&[]));
		assert_eq!(diff, vec!["- A=1".to_string(), "- B=2".to_string()]);
	}

	#[test]
	fn env_diff_hides_values_for_host_referenced_keys() {
		let current: Vec<String> = vec![
			"SECRET=old-secret-value".to_string(),
			"LITERAL=old-literal".to_string(),
		];
		let desired: Vec<String> = vec![
			"SECRET=new-secret-value".to_string(),
			"LITERAL=new-literal".to_string(),
		];
		let sensitive = managed_set(&["SECRET"]);
		let diff = env_diff(&desired, &current, &sensitive);
		assert!(
			diff.contains(&"- SECRET".to_string()),
			"sensitive removed value must be hidden: {diff:?}"
		);
		assert!(
			diff.contains(&"+ SECRET".to_string()),
			"sensitive added value must be hidden: {diff:?}"
		);
		assert!(
			!diff.iter().any(|line| line.contains("secret-value")),
			"resolved secret values must never appear: {diff:?}"
		);
		assert!(
			diff.contains(&"- LITERAL=old-literal".to_string()),
			"literal values stay visible: {diff:?}"
		);
		assert!(
			diff.contains(&"+ LITERAL=new-literal".to_string()),
			"literal values stay visible: {diff:?}"
		);
	}

	#[test]
	fn env_diff_hides_sensitive_values_in_plain_add_and_remove() {
		let sensitive = managed_set(&["SECRET"]);
		let added = env_diff(&["SECRET=abc123".to_string()], &[], &sensitive);
		assert_eq!(added, vec!["+ SECRET".to_string()]);
		let removed = env_diff(&[], &["SECRET=abc123".to_string()], &sensitive);
		assert_eq!(removed, vec!["- SECRET".to_string()]);
	}

	#[test]
	fn entries_to_env_serializes_envvar_objects() {
		let entries = vec![
			"SVAMM_REGISTRY_PROXY_PORT=50001".to_string(),
			"HI=works : )".to_string(),
		];
		let env = entries_to_env(&entries);
		assert_eq!(env[0].key, "SVAMM_REGISTRY_PROXY_PORT");
		assert_eq!(env[0].value, "50001");
		assert_eq!(env[1].key, "HI");
		assert_eq!(env[1].value, "works : )");
		// the serialized shape deserializes as microsandbox `EnvVar`.
		let parsed: Vec<EnvVar> =
			serde_json::from_value(serde_json::to_value(&env).unwrap()).unwrap();
		assert_eq!(parsed.len(), 2);
	}

	#[test]
	fn entries_to_env_keeps_equals_in_values() {
		let entries = vec!["URL=http://h/p?a=b".to_string()];
		let env = entries_to_env(&entries);
		assert_eq!(env[0].key, "URL");
		assert_eq!(env[0].value, "http://h/p?a=b");
	}

	#[test]
	fn current_env_entries_reads_envvar_objects() {
		let config: SandboxConfig = serde_json::from_value(serde_json::json!({
			"env": [
				{"key": "A", "value": "1"},
				{"key": "C", "value": "x=y"},
			]
		}))
		.unwrap();
		assert_eq!(
			current_env_entries(&config),
			vec!["A=1".to_string(), "C=x=y".to_string()]
		);
	}

	#[test]
	fn current_env_entries_missing_env_is_empty() {
		let config: SandboxConfig = serde_json::from_value(serde_json::json!({})).unwrap();
		assert!(current_env_entries(&config).is_empty());
	}

	#[test]
	fn env_key_extracts_the_variable_name() {
		assert_eq!(env_key("A=1"), "A");
		assert_eq!(env_key("URL=http://h/p?a=b"), "URL");
		assert_eq!(env_key("BROKEN"), "BROKEN");
	}

	fn managed_set(keys: &[&str]) -> BTreeSet<String> {
		keys.iter().map(|key| key.to_string()).collect()
	}

	#[test]
	fn merged_env_preserves_foreign_entries_and_applies_desired() {
		// PATH/LANG/TERM are foreign (not previously managed, not desired):
		// they must keep their values. proxy + project entries apply after.
		let current = vec![
			"PATH=/usr/bin".to_string(),
			"LANG=en_US.UTF-8".to_string(),
			"SVAMM_REGISTRY_PROXY_PORT=50001".to_string(),
			"OLD=gone".to_string(),
		];
		let previously_managed = managed_set(&["SVAMM_REGISTRY_PROXY_PORT", "OLD"]);
		let desired = vec![
			"SVAMM_REGISTRY_PROXY_PORT=50001".to_string(),
			"MY_KEY=value".to_string(),
		];
		assert_eq!(
			merged_env(&current, &previously_managed, &desired),
			vec![
				"PATH=/usr/bin".to_string(),
				"LANG=en_US.UTF-8".to_string(),
				"SVAMM_REGISTRY_PROXY_PORT=50001".to_string(),
				"MY_KEY=value".to_string(),
			],
			"foreign entries must stay, previously-managed removed keys must drop"
		);
	}

	#[test]
	fn merged_env_replaces_changed_project_keys_without_duplicates() {
		let current = vec!["PATH=/usr/bin".to_string(), "MY_KEY=old-value".to_string()];
		let previously_managed = managed_set(&["MY_KEY", "SVAMM_REGISTRY_PROXY_PORT"]);
		let desired = vec![
			"SVAMM_REGISTRY_PROXY_PORT=50001".to_string(),
			"MY_KEY=new-value".to_string(),
		];
		let env = merged_env(&current, &previously_managed, &desired);
		assert_eq!(
			env.iter()
				.filter(|entry| env_key(entry) == "MY_KEY")
				.count(),
			1,
			"a key may appear only once: {env:?}"
		);
		assert!(
			env.contains(&"MY_KEY=new-value".to_string()),
			"env: {env:?}"
		);
		assert!(
			!env.contains(&"MY_KEY=old-value".to_string()),
			"env: {env:?}"
		);
		assert!(env.contains(&"PATH=/usr/bin".to_string()), "env: {env:?}");
	}

	#[test]
	fn merged_env_without_metadata_preserves_all_non_desired_keys() {
		// first run on a pre-feature sandbox: nothing was previously managed,
		// so every persisted key the project does not declare stays.
		let current = vec![
			"PATH=/usr/bin".to_string(),
			"LANG=en_US.UTF-8".to_string(),
			"SVAMM_REGISTRY_PROXY_PORT=50001".to_string(),
		];
		let previously_managed = managed_set(&[]);
		let desired = vec![
			"SVAMM_REGISTRY_PROXY_PORT=50001".to_string(),
			"MY_KEY=value".to_string(),
		];
		assert_eq!(
			merged_env(&current, &previously_managed, &desired),
			vec![
				"PATH=/usr/bin".to_string(),
				"LANG=en_US.UTF-8".to_string(),
				"SVAMM_REGISTRY_PROXY_PORT=50001".to_string(),
				"MY_KEY=value".to_string(),
			]
		);
	}

	#[test]
	fn merged_env_empty_desired_keeps_foreign_and_drops_managed() {
		let current = vec![
			"PATH=/usr/bin".to_string(),
			"MY_KEY=value".to_string(),
			"SECRET=old".to_string(),
		];
		let previously_managed = managed_set(&["MY_KEY", "SECRET"]);
		assert_eq!(
			merged_env(&current, &previously_managed, &[]),
			vec!["PATH=/usr/bin".to_string()],
			"an empty envs object must drop the managed keys and keep foreign ones"
		);
	}

	#[test]
	fn drop_managed_env_keys_removes_only_previously_managed_keys() {
		let current = vec![
			"PATH=/usr/bin".to_string(),
			"SVAMM_REGISTRY_PROXY_PORT=50001".to_string(),
			"MY_KEY=value".to_string(),
		];
		let previously_managed = managed_set(&["MY_KEY"]);
		assert_eq!(
			drop_managed_env_keys(&current, &previously_managed),
			vec![
				"PATH=/usr/bin".to_string(),
				"SVAMM_REGISTRY_PROXY_PORT=50001".to_string(),
			],
			"previously-managed keys drop while foreign and proxy entries stay"
		);
	}

	#[test]
	fn drop_managed_env_keys_never_drops_the_proxy_var() {
		// metadata recorded by older builds may list the proxy var: the proxy
		// must survive a no-envs cleanup regardless.
		let current = vec![
			"SVAMM_REGISTRY_PROXY_PORT=50001".to_string(),
			"MY_KEY=value".to_string(),
		];
		let previously_managed = managed_set(&["SVAMM_REGISTRY_PROXY_PORT", "MY_KEY"]);
		assert_eq!(
			drop_managed_env_keys(&current, &previously_managed),
			vec!["SVAMM_REGISTRY_PROXY_PORT=50001".to_string()],
			"the generated proxy var must never be dropped by an envs cleanup"
		);
	}

	#[test]
	fn drop_managed_env_keys_untouched_when_nothing_managed() {
		let current = vec![
			"PATH=/usr/bin".to_string(),
			"SVAMM_REGISTRY_PROXY_PORT=50001".to_string(),
		];
		assert_eq!(drop_managed_env_keys(&current, &managed_set(&[])), current);
	}

	#[test]
	fn net_rule_diff_reports_added_and_removed() {
		let current: Vec<microsandbox_types::Rule> =
			vec![parse_rule("allow@host:tcp:5432"), parse_rule("deny@x.net")];
		let desired: Vec<microsandbox_types::Rule> = vec![
			parse_rule("allow@host:tcp:5432"),
			parse_rule("allow@svamm-project.test:tcp:443"),
		];
		let diff = net_rule_diff(&desired, &current);
		assert!(
			diff.iter().any(|line| line == "- deny@x.net"),
			"removed rule must be reported: {diff:?}"
		);
		assert!(
			diff.iter()
				.any(|line| line == "+ allow@svamm-project.test:tcp:443"),
			"added rule must be reported: {diff:?}"
		);
		assert!(
			diff.iter()
				.all(|line| !line.contains("allow@host:tcp:5432")),
			"unchanged rule must not appear: {diff:?}"
		);
	}

	#[test]
	fn mount_diff_reports_removed_and_added() {
		let owned: BTreeSet<&str> = ["/home/agent/proj", "/home/agent/ref"]
			.into_iter()
			.collect();
		let persisted = vec![
			bind_mount("/srv/proj", "/home/agent/proj", false, Some(32768)),
			bind_mount("/srv/ref", "/home/agent/ref", true, None),
		];
		let desired = vec![bind_mount(
			"/srv/proj",
			"/home/agent/proj",
			false,
			Some(32768),
		)];
		let diff = mount_diff_lines(&persisted, &desired, &owned);
		assert!(
			diff.iter()
				.any(|line| line == "- /srv/ref:/home/agent/ref:ro"),
			"removed mount must be reported: {diff:?}"
		);
		assert!(
			!diff.iter().any(|line| line.contains("/home/agent/proj")),
			"unchanged mount must not appear: {diff:?}"
		);
		let added = mount_diff_lines(
			&[],
			&[bind_mount("/srv/new", "/home/agent/new", true, None)],
			&owned,
		);
		assert!(
			added
				.iter()
				.any(|line| line == "+ /srv/new:/home/agent/new:ro"),
			"added mount must be reported: {added:?}"
		);
	}

	#[test]
	fn mount_guest_returns_some_for_known_shapes() {
		assert_eq!(
			mount_guest(&bind_mount("/srv/a", "/home/agent/a", true, None)),
			Some("/home/agent/a")
		);
	}
}
