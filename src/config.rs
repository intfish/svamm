//! svamm global configuration (`~/.config/svamm/config.toml`).

use std::path::PathBuf;

use microsandbox_cli::commands::common::SandboxConfigSources;
use serde::{Deserialize, Serialize};

use crate::paths;
use crate::validate::require_quota_size;

/// the default public image repository for svamm base images.
const PUBLIC_IMAGE_REPO: &str = "ghcr.io/intfish/svamm-images";

/// the svamm release version, from the crate version in `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// label key applied to svamm-managed sandboxes.
pub const MANAGED_LABEL_KEY: &str = "svamm";
pub const MANAGED_LABEL_VALUE: &str = "managed";

/// label applied to the registry sandbox.
///
/// it intentionally differs from `svamm=managed`: `svamm list` and
/// `svamm stop -a` skip the registry so it keeps serving images.
pub const REGISTRY_LABEL: &str = "svamm=registry";

/// guest user created by the svamm vm image.
pub const AGENT_HOME_DEFAULT: &str = "/home/agent";
pub const AGENT_SHELL_DEFAULT: &str = "zsh";

/// fully resolved svamm configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
	/// configuration schema version.
	pub version: u32,
	/// local image registry settings.
	pub registry: Registry,
	/// agent user defaults inside the sandbox.
	pub agent: Agent,
	/// sandbox creation defaults.
	pub sandbox: Sandbox,
	/// image references for the images svamm loads.
	pub images: Images,
	/// locations of the msb-facing sparse config files.
	pub configs: MsbConfigs,
}

/// the svamm-managed local image registry (a dedicated sandbox).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Registry {
	/// sandbox name for the registry vm.
	pub name: String,
	/// host loopback port the registry publishes on (host-only admin access).
	pub host_port: u16,
	/// guest port the registry listens on.
	pub guest_port: u16,
	/// host loopback port the read-only nginx proxy publishes on.
	///
	/// sandboxes reach the registry only through this proxy. writes from
	/// inside a sandbox are refused with 405.
	pub proxy_port: u16,
	/// host directory persisted into the registry vm at `/var/lib/registry`.
	pub data_dir: String,
	/// guest-write quota for the registry data mount.
	pub quota: String,
	/// how long commands wait for the registry to answer after starting it.
	/// zero disables the wait.
	pub ready_timeout_ms: u64,
	/// repository name used for the svamm image inside the registry.
	pub repo: String,
}

/// agent user defaults inside sandboxes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Agent {
	/// guest home directory.
	pub home: String,
	/// guest shell for interactive sessions.
	pub shell: String,
}

/// sandbox creation defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Sandbox {
	/// flat root disk size for oci sandboxes.
	pub root_disk_size: String,
	/// guest-write quota for rw workspace mounts.
	pub workspace_quota: String,
	/// guest destination of the tmpfs mount.
	pub tmpfs_dest: String,
	/// size of the tmpfs mount.
	pub tmpfs_size: String,
	/// virtual cpus to allocate. unset: the msb `resources.yml` value applies.
	pub cpus: Option<u8>,
	/// burstable cpu ceiling. unset: identical to `cpus`.
	pub max_cpus: Option<u8>,
	/// memory to allocate. unset: the msb `resources.yml` value applies.
	pub memory: Option<String>,
	/// burstable memory ceiling. unset: identical to `memory`.
	pub max_memory: Option<String>,
}

/// image references for the images svamm loads.
///
/// both default to the public base images published on ghcr.io.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Images {
	/// default sandbox (vm) image reference. required.
	///
	/// sandboxes boot from this image. the per-project `svamm.json` `image`
	/// override takes precedence. an empty value is rejected by
	/// `Config::validate`.
	pub vm: String,
	/// image reference for the local registry sandbox.
	///
	/// svamm pulls this image into the microsandbox cache instead of building it.
	pub registry: String,
}

/// locations of the msb-facing sparse configuration files.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MsbConfigs {
	/// root sparse sandbox config (env/init).
	pub base: String,
	/// resource config.
	pub resources: String,
	/// network config.
	pub network: String,
	/// base allow/deny network rules.
	pub net_rules: String,
	/// resource config for the registry sandbox.
	pub registry_resources: String,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			version: 1,
			registry: Registry::default(),
			agent: Agent::default(),
			sandbox: Sandbox::default(),
			images: Images::default(),
			configs: MsbConfigs::default(),
		}
	}
}

impl Default for Registry {
	fn default() -> Self {
		let port = env_u16("SVAMM_REGISTRY_PORT", 50000);
		let proxy_port = env_u16("SVAMM_REGISTRY_PROXY_PORT", 50001);
		let name = env_str("SVAMM_REGISTRY_NAME", "svamm-registry");
		let data = env_str("SVAMM_REGISTRY_DATA", "");
		let quota = env_str("SVAMM_REGISTRY_QUOTA", "32G");
		let ready_timeout_ms = env_u64("SVAMM_REGISTRY_READY_TIMEOUT_MS", 30_000);
		Self {
			name,
			host_port: port,
			guest_port: 5000,
			proxy_port,
			data_dir: if data.is_empty() {
				paths::state_dir().join("registry").display().to_string()
			} else {
				data
			},
			quota,
			ready_timeout_ms,
			repo: "svamm".to_string(),
		}
	}
}

impl Default for Agent {
	fn default() -> Self {
		Self {
			home: AGENT_HOME_DEFAULT.to_string(),
			shell: AGENT_SHELL_DEFAULT.to_string(),
		}
	}
}

impl Default for Sandbox {
	fn default() -> Self {
		Self {
			root_disk_size: env_str("SVAMM_ROOT_DISK_SIZE", "32G"),
			workspace_quota: env_str("SVAMM_WORKSPACE_QUOTA", "32G"),
			tmpfs_dest: "/tmp".to_string(),
			tmpfs_size: env_str("SVAMM_TMPFS_SIZE", "2G"),
			cpus: env_u8_opt("SVAMM_CPUS"),
			max_cpus: env_u8_opt("SVAMM_MAX_CPUS"),
			memory: env_opt("SVAMM_MEMORY"),
			max_memory: env_opt("SVAMM_MAX_MEMORY"),
		}
	}
}

impl Default for Images {
	fn default() -> Self {
		Self {
			vm: env_str("SVAMM_IMAGES_VM", &public_image_ref("svamm")),
			registry: env_str("SVAMM_IMAGES_REGISTRY", &public_image_ref("registry")),
		}
	}
}

impl Default for MsbConfigs {
	fn default() -> Self {
		let dir = paths::config_dir();
		Self {
			base: dir.join("base.yml").display().to_string(),
			resources: dir.join("resources.yml").display().to_string(),
			network: dir.join("network.yml").display().to_string(),
			net_rules: dir.join("net-rules.json").display().to_string(),
			registry_resources: dir.join("registry-resources.yml").display().to_string(),
		}
	}
}

impl Config {
	/// load configuration from `{config_dir}/config.toml`, applying environment
	/// overrides on top. a missing file falls back to defaults; a file that
	/// cannot be read or parsed fails loudly.
	pub fn load() -> anyhow::Result<Self> {
		let path = paths::config_dir().join("config.toml");
		let text = match std::fs::read_to_string(&path) {
			Ok(text) => text,
			Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
			Err(err) => anyhow::bail!("failed to read {}: {err}", path.display()),
		};
		let mut config: Config = toml::from_str(&text)
			.map_err(|err| anyhow::anyhow!("failed to parse {}: {err}", path.display()))?;
		config.apply_env();
		Ok(config)
	}

	/// the resolved registry data directory.
	pub fn registry_data_dir(&self) -> PathBuf {
		paths::expand_tilde(&self.registry.data_dir)
	}

	/// the msb global config file path.
	pub fn msb_config_path(&self) -> PathBuf {
		let overridden = env_str("SVAMM_MSB_CONFIG", "");
		if !overridden.is_empty() {
			return paths::expand_tilde(&overridden);
		}
		paths::msb_home().join("config.json")
	}

	/// the image reference used for sandboxes.
	///
	/// always the configured `images.vm`. the per-project `svamm.json` `image`
	/// override takes precedence at create time.
	pub fn sandbox_image_ref(&self) -> String {
		self.images.vm.clone()
	}

	/// the registry reference prefix used when talking to the local registry.
	pub fn registry_ref_prefix(&self) -> String {
		format!(
			"127.0.0.1:{}/{}",
			self.registry.host_port, self.registry.repo
		)
	}

	/// the host address of the published registry port.
	pub fn registry_host(&self) -> String {
		format!("127.0.0.1:{}", self.registry.host_port)
	}

	/// the host address of the published read-only registry proxy port.
	pub fn registry_proxy_host(&self) -> String {
		format!("127.0.0.1:{}", self.registry.proxy_port)
	}

	/// the registry hosts referenced by the configured images that msb must
	/// treat as insecure.
	///
	/// svamm marks hosts with an explicit non-443 port, assuming they are
	/// plain http. hosts without a port stay https.
	pub fn insecure_image_registry_hosts(&self) -> Vec<String> {
		let mut hosts: Vec<String> = Vec::new();
		for reference in [&self.images.vm, &self.images.registry] {
			let reference = reference.trim();
			if reference.is_empty() {
				continue;
			}
			let Ok(parsed) = reference.parse::<microsandbox_image::Reference>() else {
				continue;
			};
			let host = parsed.registry().to_string();
			if is_insecure_registry_host(&host) && !hosts.contains(&host) {
				hosts.push(host);
			}
		}
		hosts
	}

	/// the image reference used for the registry sandbox itself.
	pub fn registry_image_ref(&self) -> String {
		self.images.registry.clone()
	}

	/// the label selector used to select svamm-managed sandboxes.
	pub fn managed_label(&self) -> String {
		format!("{MANAGED_LABEL_KEY}={MANAGED_LABEL_VALUE}")
	}

	/// validate cross-field invariants that would otherwise fail confusingly
	/// later.
	pub fn validate(&self) -> anyhow::Result<()> {
		if self.registry.proxy_port == self.registry.host_port {
			anyhow::bail!(
				"registry proxy port ({}) must differ from the registry port ({})",
				self.registry.proxy_port,
				self.registry.host_port
			);
		}
		require_quota_size(&self.registry.quota).map_err(anyhow::Error::msg)?;
		for (name, reference) in [
			("images.vm", &self.images.vm),
			("images.registry", &self.images.registry),
		] {
			if reference.trim().is_empty() {
				anyhow::bail!("{name}: must be set to a non-empty image reference");
			}
			reference
				.parse::<microsandbox_image::Reference>()
				.map_err(|err| {
					anyhow::anyhow!("{name}: invalid image reference '{reference}': {err}")
				})?;
		}
		Ok(())
	}

	fn apply_env(&mut self) {
		let port = env_u16("SVAMM_REGISTRY_PORT", self.registry.host_port);
		self.registry.host_port = port;
		let proxy_port = env_u16("SVAMM_REGISTRY_PROXY_PORT", self.registry.proxy_port);
		self.registry.proxy_port = proxy_port;
		let name = env_str("SVAMM_REGISTRY_NAME", &self.registry.name);
		self.registry.name = name;
		let data = env_str("SVAMM_REGISTRY_DATA", &self.registry.data_dir);
		if !data.is_empty() {
			self.registry.data_dir = data;
		}
		let quota = env_str("SVAMM_REGISTRY_QUOTA", &self.registry.quota);
		self.registry.quota = quota;
		let ready_timeout_ms = env_u64(
			"SVAMM_REGISTRY_READY_TIMEOUT_MS",
			self.registry.ready_timeout_ms,
		);
		self.registry.ready_timeout_ms = ready_timeout_ms;
		let vm = env_str("SVAMM_IMAGES_VM", &self.images.vm);
		self.images.vm = vm;
		let registry = env_str("SVAMM_IMAGES_REGISTRY", &self.images.registry);
		self.images.registry = registry;
		let disk = env_str("SVAMM_ROOT_DISK_SIZE", &self.sandbox.root_disk_size);
		self.sandbox.root_disk_size = disk;
		let quota = env_str("SVAMM_WORKSPACE_QUOTA", &self.sandbox.workspace_quota);
		self.sandbox.workspace_quota = quota;
		let tmpfs_size = env_str("SVAMM_TMPFS_SIZE", &self.sandbox.tmpfs_size);
		self.sandbox.tmpfs_size = tmpfs_size;
		if let Some(cpus) = env_u8_opt("SVAMM_CPUS") {
			self.sandbox.cpus = Some(cpus);
		}
		if let Some(max_cpus) = env_u8_opt("SVAMM_MAX_CPUS") {
			self.sandbox.max_cpus = Some(max_cpus);
		}
		if let Some(memory) = env_opt("SVAMM_MEMORY") {
			self.sandbox.memory = Some(memory);
		}
		if let Some(max_memory) = env_opt("SVAMM_MAX_MEMORY") {
			self.sandbox.max_memory = Some(max_memory);
		}
	}
}

/// true when running in dry-run mode (`SVAMM_DRY_RUN=1`).
pub fn dry_run() -> bool {
	std::env::var("SVAMM_DRY_RUN").as_deref() == Ok("1")
}

/// build `SandboxConfigSources` from an ordered list of `(flag, path)` pairs.
pub fn build_config_sources(entries: &[(&str, PathBuf)]) -> anyhow::Result<SandboxConfigSources> {
	use clap::{Args, Command, FromArgMatches};

	let command = SandboxConfigSources::augment_args(Command::new("sources"));
	let mut argv: Vec<std::ffi::OsString> = vec!["sources".into()];
	for (flag, path) in entries {
		argv.push(flag.into());
		argv.push(path.as_os_str().to_owned());
	}
	let matches = command
		.try_get_matches_from(argv)
		.map_err(|err| anyhow::anyhow!("failed to build config sources: {err}"))?;
	SandboxConfigSources::from_arg_matches(&matches)
		.map_err(|err| anyhow::anyhow!("failed to build config sources: {err}"))
}

/// default config source entries in msb-compatible order.
pub fn default_config_entries(config: &Config) -> Vec<(&'static str, PathBuf)> {
	vec![
		("--conf", paths::expand_tilde(&config.configs.base)),
		(
			"--resource-conf",
			paths::expand_tilde(&config.configs.resources),
		),
		("--net-conf", paths::expand_tilde(&config.configs.network)),
	]
}

fn env_str(key: &str, default: &str) -> String {
	std::env::var(key)
		.ok()
		.filter(|value| !value.is_empty())
		.unwrap_or_else(|| default.to_string())
}

/// a non-empty environment value, or `None` when unset/empty.
fn env_opt(key: &str) -> Option<String> {
	std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// a parseable `u8` environment value for cpu counts, or `None` otherwise.
fn env_u8_opt(key: &str) -> Option<u8> {
	env_opt(key)?.parse().ok()
}

/// true when a registry host is served over plain http.
///
/// hosts with an explicit port other than 443 are treated as insecure. hosts
/// without a port are assumed to be https.
fn is_insecure_registry_host(host: &str) -> bool {
	match host.rsplit_once(':') {
		Some((_, port)) => port.parse::<u16>().map(|port| port != 443).unwrap_or(false),
		None => false,
	}
}

/// the public ghcr.io image reference for a named svamm base image.
fn public_image_ref(kind: &str) -> String {
	format!("{PUBLIC_IMAGE_REPO}/{kind}:latest")
}

fn env_u16(key: &str, default: u16) -> u16 {
	std::env::var(key)
		.ok()
		.and_then(|value| value.parse().ok())
		.unwrap_or(default)
}

/// a parseable `u64` environment value, or `default` otherwise.
fn env_u64(key: &str, default: u64) -> u64 {
	std::env::var(key)
		.ok()
		.and_then(|value| value.parse().ok())
		.unwrap_or(default)
}

/// the effective strict-hostname-policy value for sandboxes.
pub fn network_strict(config: &Config) -> Result<bool, String> {
	let path = paths::expand_tilde(&config.configs.network);
	let text = match std::fs::read_to_string(&path) {
		Ok(text) => text,
		Err(_err) if !path.exists() => crate::assets::embedded("network.yml")
			.map(str::to_string)
			.unwrap_or_default(),
		Err(err) => return Err(format!("failed to read {}: {err}", path.display())),
	};
	parse_network_strict(&text).map(|value| value.unwrap_or(false))
}

/// parse the top-level `strict:` value from a sparse network config.
fn parse_network_strict(text: &str) -> Result<Option<bool>, String> {
	for line in text.lines() {
		let line = line.trim_start();
		let Some(rest) = line.strip_prefix("strict:") else {
			continue;
		};
		let value = rest.split('#').next().unwrap_or_default().trim();
		let value = value.trim_matches(['"', '\'']);
		return match value {
			"true" => Ok(Some(true)),
			"false" => Ok(Some(false)),
			other => Err(format!(
				"network.yml: invalid strict value '{other}'; expected true or false"
			)),
		};
	}
	Ok(None)
}

#[cfg(test)]
mod tests {
	use super::*;

	// serialize tests that mutate the process env.
	static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

	#[test]
	fn default_sandbox_image_ref_uses_public_vm_image() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original_port = std::env::var_os("SVAMM_REGISTRY_PORT");
		let original_vm = std::env::var_os("SVAMM_IMAGES_VM");
		unsafe { std::env::remove_var("SVAMM_REGISTRY_PORT") };
		unsafe { std::env::remove_var("SVAMM_IMAGES_VM") };
		let config = Config::default();
		assert_eq!(
			config.sandbox_image_ref(),
			"ghcr.io/intfish/svamm-images/svamm:latest".to_string()
		);
		assert_eq!(config.managed_label(), "svamm=managed");
		match original_port {
			Some(value) => unsafe { std::env::set_var("SVAMM_REGISTRY_PORT", value) },
			None => unsafe { std::env::remove_var("SVAMM_REGISTRY_PORT") },
		}
		match original_vm {
			Some(value) => unsafe { std::env::set_var("SVAMM_IMAGES_VM", value) },
			None => unsafe { std::env::remove_var("SVAMM_IMAGES_VM") },
		}
	}

	#[test]
	fn empty_images_vm_is_rejected_by_validate() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original_port = std::env::var_os("SVAMM_REGISTRY_PORT");
		let original_vm = std::env::var_os("SVAMM_IMAGES_VM");
		unsafe { std::env::set_var("SVAMM_REGISTRY_PORT", "50000") };
		unsafe { std::env::remove_var("SVAMM_IMAGES_VM") };
		let mut config = Config::default();
		config.images.vm = String::new();
		let err = config.validate().unwrap_err().to_string();
		assert!(err.contains("images.vm"), "unexpected error: {err}");
		assert!(err.contains("must be set"), "unexpected error: {err}");
		match original_port {
			Some(value) => unsafe { std::env::set_var("SVAMM_REGISTRY_PORT", value) },
			None => unsafe { std::env::remove_var("SVAMM_REGISTRY_PORT") },
		}
		match original_vm {
			Some(value) => unsafe { std::env::set_var("SVAMM_IMAGES_VM", value) },
			None => unsafe { std::env::remove_var("SVAMM_IMAGES_VM") },
		}
	}

	#[test]
	fn registry_env_overrides_apply() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original_port = std::env::var_os("SVAMM_REGISTRY_PORT");
		let original_name = std::env::var_os("SVAMM_REGISTRY_NAME");
		unsafe { std::env::set_var("SVAMM_REGISTRY_PORT", "59999") };
		unsafe { std::env::set_var("SVAMM_REGISTRY_NAME", "reg-x") };
		let config = Config::default();
		assert_eq!(config.registry.host_port, 59999);
		assert_eq!(config.registry.name, "reg-x");
		match original_port {
			Some(value) => unsafe { std::env::set_var("SVAMM_REGISTRY_PORT", value) },
			None => unsafe { std::env::remove_var("SVAMM_REGISTRY_PORT") },
		}
		match original_name {
			Some(value) => unsafe { std::env::set_var("SVAMM_REGISTRY_NAME", value) },
			None => unsafe { std::env::remove_var("SVAMM_REGISTRY_NAME") },
		}
	}

	#[test]
	fn workspace_quota_defaults_and_env_override_apply() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original = std::env::var_os("SVAMM_WORKSPACE_QUOTA");
		unsafe { std::env::remove_var("SVAMM_WORKSPACE_QUOTA") };
		assert_eq!(Config::default().sandbox.workspace_quota, "32G");
		unsafe { std::env::set_var("SVAMM_WORKSPACE_QUOTA", "64G") };
		assert_eq!(Config::default().sandbox.workspace_quota, "64G");
		match original {
			Some(value) => unsafe { std::env::set_var("SVAMM_WORKSPACE_QUOTA", value) },
			None => unsafe { std::env::remove_var("SVAMM_WORKSPACE_QUOTA") },
		}
	}

	#[test]
	fn registry_quota_defaults_and_env_override_apply() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original = std::env::var_os("SVAMM_REGISTRY_QUOTA");
		unsafe { std::env::remove_var("SVAMM_REGISTRY_QUOTA") };
		assert_eq!(Config::default().registry.quota, "32G");
		unsafe { std::env::set_var("SVAMM_REGISTRY_QUOTA", "128G") };
		assert_eq!(Config::default().registry.quota, "128G");
		match original {
			Some(value) => unsafe { std::env::set_var("SVAMM_REGISTRY_QUOTA", value) },
			None => unsafe { std::env::remove_var("SVAMM_REGISTRY_QUOTA") },
		}
	}

	#[test]
	fn registry_ready_timeout_defaults_and_env_override_apply() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original = std::env::var_os("SVAMM_REGISTRY_READY_TIMEOUT_MS");
		unsafe { std::env::remove_var("SVAMM_REGISTRY_READY_TIMEOUT_MS") };
		assert_eq!(Config::default().registry.ready_timeout_ms, 30_000);
		unsafe { std::env::set_var("SVAMM_REGISTRY_READY_TIMEOUT_MS", "500") };
		assert_eq!(Config::default().registry.ready_timeout_ms, 500);
		match original {
			Some(value) => unsafe { std::env::set_var("SVAMM_REGISTRY_READY_TIMEOUT_MS", value) },
			None => unsafe { std::env::remove_var("SVAMM_REGISTRY_READY_TIMEOUT_MS") },
		}
	}

	#[test]
	fn root_disk_size_defaults_to_32g_and_env_override_apply() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original = std::env::var_os("SVAMM_ROOT_DISK_SIZE");
		unsafe { std::env::remove_var("SVAMM_ROOT_DISK_SIZE") };
		assert_eq!(Config::default().sandbox.root_disk_size, "32G");
		unsafe { std::env::set_var("SVAMM_ROOT_DISK_SIZE", "64G") };
		assert_eq!(Config::default().sandbox.root_disk_size, "64G");
		match original {
			Some(value) => unsafe { std::env::set_var("SVAMM_ROOT_DISK_SIZE", value) },
			None => unsafe { std::env::remove_var("SVAMM_ROOT_DISK_SIZE") },
		}
	}

	#[test]
	fn tmpfs_size_defaults_and_env_override_apply() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original = std::env::var_os("SVAMM_TMPFS_SIZE");
		unsafe { std::env::remove_var("SVAMM_TMPFS_SIZE") };
		assert_eq!(Config::default().sandbox.tmpfs_size, "2G");
		unsafe { std::env::set_var("SVAMM_TMPFS_SIZE", "4G") };
		assert_eq!(Config::default().sandbox.tmpfs_size, "4G");
		match original {
			Some(value) => unsafe { std::env::set_var("SVAMM_TMPFS_SIZE", value) },
			None => unsafe { std::env::remove_var("SVAMM_TMPFS_SIZE") },
		}
	}

	#[test]
	fn sandbox_resources_default_to_unset_and_env_overrides_apply() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original = [
			"SVAMM_CPUS",
			"SVAMM_MAX_CPUS",
			"SVAMM_MEMORY",
			"SVAMM_MAX_MEMORY",
		]
		.map(|key| (key, std::env::var_os(key)));
		for (key, _) in &original {
			unsafe { std::env::remove_var(key) };
		}
		let default = Config::default();
		assert!(default.sandbox.cpus.is_none());
		assert!(default.sandbox.max_cpus.is_none());
		assert!(default.sandbox.memory.is_none());
		assert!(default.sandbox.max_memory.is_none());
		unsafe {
			std::env::set_var("SVAMM_CPUS", "4");
			std::env::set_var("SVAMM_MAX_CPUS", "8");
			std::env::set_var("SVAMM_MEMORY", "8G");
			std::env::set_var("SVAMM_MAX_MEMORY", "16G");
		}
		let config = Config::default();
		assert_eq!(config.sandbox.cpus, Some(4));
		assert_eq!(config.sandbox.max_cpus, Some(8));
		assert_eq!(config.sandbox.memory.as_deref(), Some("8G"));
		assert_eq!(config.sandbox.max_memory.as_deref(), Some("16G"));
		for (key, value) in original {
			match value {
				Some(value) => unsafe { std::env::set_var(key, value) },
				None => unsafe { std::env::remove_var(key) },
			}
		}
	}

	#[test]
	fn proxy_port_defaults_and_env_override_apply() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original = std::env::var_os("SVAMM_REGISTRY_PROXY_PORT");
		unsafe { std::env::remove_var("SVAMM_REGISTRY_PROXY_PORT") };
		assert_eq!(Config::default().registry.proxy_port, 50001);
		unsafe { std::env::set_var("SVAMM_REGISTRY_PROXY_PORT", "51111") };
		assert_eq!(Config::default().registry.proxy_port, 51111);
		match original {
			Some(value) => unsafe { std::env::set_var("SVAMM_REGISTRY_PROXY_PORT", value) },
			None => unsafe { std::env::remove_var("SVAMM_REGISTRY_PROXY_PORT") },
		}
	}

	#[test]
	fn registry_image_ref_uses_public_registry_image() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original_port = std::env::var_os("SVAMM_REGISTRY_PORT");
		let original_registry = std::env::var_os("SVAMM_IMAGES_REGISTRY");
		unsafe { std::env::set_var("SVAMM_REGISTRY_PORT", "55555") };
		unsafe { std::env::remove_var("SVAMM_IMAGES_REGISTRY") };
		let config = Config::default();
		assert_eq!(
			config.registry_image_ref(),
			"ghcr.io/intfish/svamm-images/registry:latest".to_string()
		);
		assert_eq!(config.registry_proxy_host(), "127.0.0.1:50001");
		match original_port {
			Some(value) => unsafe { std::env::set_var("SVAMM_REGISTRY_PORT", value) },
			None => unsafe { std::env::remove_var("SVAMM_REGISTRY_PORT") },
		}
		match original_registry {
			Some(value) => unsafe { std::env::set_var("SVAMM_IMAGES_REGISTRY", value) },
			None => unsafe { std::env::remove_var("SVAMM_IMAGES_REGISTRY") },
		}
	}

	#[test]
	fn registry_image_ref_mirrors_configured_registry_image() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original_registry = std::env::var_os("SVAMM_IMAGES_REGISTRY");
		unsafe {
			std::env::set_var("SVAMM_IMAGES_REGISTRY", "127.0.0.1:55555/svamm-registry:1");
		}
		let config = Config::default();
		assert_eq!(
			config.registry_image_ref(),
			"127.0.0.1:55555/svamm-registry:1"
		);
		match original_registry {
			Some(value) => unsafe { std::env::set_var("SVAMM_IMAGES_REGISTRY", value) },
			None => unsafe { std::env::remove_var("SVAMM_IMAGES_REGISTRY") },
		}
	}

	#[test]
	fn images_env_overrides_apply() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original_vm = std::env::var_os("SVAMM_IMAGES_VM");
		let original_registry = std::env::var_os("SVAMM_IMAGES_REGISTRY");
		unsafe {
			std::env::set_var("SVAMM_IMAGES_VM", "127.0.0.1:50000/acme:latest");
			std::env::set_var("SVAMM_IMAGES_REGISTRY", "ghcr.io/example/registry:7");
		}
		let config = Config::default();
		assert_eq!(config.images.vm, "127.0.0.1:50000/acme:latest");
		assert_eq!(config.images.registry, "ghcr.io/example/registry:7");
		assert_eq!(config.sandbox_image_ref(), "127.0.0.1:50000/acme:latest");
		assert_eq!(config.registry_image_ref(), "ghcr.io/example/registry:7");
		match original_vm {
			Some(value) => unsafe { std::env::set_var("SVAMM_IMAGES_VM", value) },
			None => unsafe { std::env::remove_var("SVAMM_IMAGES_VM") },
		}
		match original_registry {
			Some(value) => unsafe { std::env::set_var("SVAMM_IMAGES_REGISTRY", value) },
			None => unsafe { std::env::remove_var("SVAMM_IMAGES_REGISTRY") },
		}
	}

	#[test]
	fn config_round_trips_images_table() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original_vm = std::env::var_os("SVAMM_IMAGES_VM");
		let original_registry = std::env::var_os("SVAMM_IMAGES_REGISTRY");
		unsafe { std::env::remove_var("SVAMM_IMAGES_VM") };
		unsafe { std::env::remove_var("SVAMM_IMAGES_REGISTRY") };
		let config = Config::default();
		let toml = toml::to_string_pretty(&config).unwrap();
		let parsed: Config = toml::from_str(&toml).unwrap();
		assert_eq!(parsed.images.vm, config.images.vm);
		assert_eq!(parsed.images.registry, config.images.registry);
		// an explicit empty value round-trips and is rejected by validate.
		let mut explicit = config;
		explicit.images.vm = String::new();
		let toml = toml::to_string_pretty(&explicit).unwrap();
		let parsed: Config = toml::from_str(&toml).unwrap();
		assert_eq!(parsed.images.vm, "");
		match original_vm {
			Some(value) => unsafe { std::env::set_var("SVAMM_IMAGES_VM", value) },
			None => unsafe { std::env::remove_var("SVAMM_IMAGES_VM") },
		}
		match original_registry {
			Some(value) => unsafe { std::env::set_var("SVAMM_IMAGES_REGISTRY", value) },
			None => unsafe { std::env::remove_var("SVAMM_IMAGES_REGISTRY") },
		}
	}

	#[test]
	fn validate_rejects_invalid_and_empty_image_refs() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original_vm = std::env::var_os("SVAMM_IMAGES_VM");
		let original_registry = std::env::var_os("SVAMM_IMAGES_REGISTRY");
		unsafe {
			std::env::set_var("SVAMM_IMAGES_VM", "not a ref!!");
			std::env::set_var("SVAMM_IMAGES_REGISTRY", "ghcr.io/example/reg:1");
		}
		let config = Config::default();
		let err = config.validate().unwrap_err().to_string();
		assert!(err.contains("images.vm"), "unexpected error: {err}");
		assert!(
			err.contains("invalid image reference"),
			"unexpected error: {err}"
		);

		// both image references are required.
		unsafe {
			std::env::set_var("SVAMM_IMAGES_VM", "ghcr.io/example/vm:1");
			std::env::set_var("SVAMM_IMAGES_REGISTRY", "");
		}
		let mut config = Config::default();
		config.images.registry = String::new();
		let err = config.validate().unwrap_err().to_string();
		assert!(err.contains("images.registry"), "unexpected error: {err}");
		assert!(err.contains("must be set"), "unexpected error: {err}");

		config.images.registry = "ghcr.io/example/registry:1".to_string();
		assert!(config.validate().is_ok());
		match original_vm {
			Some(value) => unsafe { std::env::set_var("SVAMM_IMAGES_VM", value) },
			None => unsafe { std::env::remove_var("SVAMM_IMAGES_VM") },
		}
		match original_registry {
			Some(value) => unsafe { std::env::set_var("SVAMM_IMAGES_REGISTRY", value) },
			None => unsafe { std::env::remove_var("SVAMM_IMAGES_REGISTRY") },
		}
	}

	#[test]
	fn insecure_image_registry_hosts_only_includes_http_hosts() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original_vm = std::env::var_os("SVAMM_IMAGES_VM");
		let original_registry = std::env::var_os("SVAMM_IMAGES_REGISTRY");
		unsafe {
			std::env::set_var("SVAMM_IMAGES_VM", "192.168.1.5:5000/team/vm:1");
			std::env::set_var("SVAMM_IMAGES_REGISTRY", "registry.example.com:8443/reg:1");
		}
		let config = Config::default();
		assert_eq!(
			config.insecure_image_registry_hosts(),
			vec!["192.168.1.5:5000", "registry.example.com:8443"]
		);

		// https-only hosts with port 443 are not marked insecure.
		unsafe {
			std::env::set_var(
				"SVAMM_IMAGES_VM",
				"ghcr.io/intfish/svamm-images/svamm:latest",
			);
			std::env::set_var("SVAMM_IMAGES_REGISTRY", "example.com:443/reg:1");
		}
		let config = Config::default();
		assert!(config.insecure_image_registry_hosts().is_empty());
		match original_vm {
			Some(value) => unsafe { std::env::set_var("SVAMM_IMAGES_VM", value) },
			None => unsafe { std::env::remove_var("SVAMM_IMAGES_VM") },
		}
		match original_registry {
			Some(value) => unsafe { std::env::set_var("SVAMM_IMAGES_REGISTRY", value) },
			None => unsafe { std::env::remove_var("SVAMM_IMAGES_REGISTRY") },
		}
	}

	#[test]
	fn validate_rejects_equal_registry_ports() {
		let _guard = ENV_LOCK.lock().unwrap();
		let original_port = std::env::var_os("SVAMM_REGISTRY_PORT");
		let original_proxy = std::env::var_os("SVAMM_REGISTRY_PROXY_PORT");
		unsafe { std::env::set_var("SVAMM_REGISTRY_PORT", "50000") };
		unsafe { std::env::set_var("SVAMM_REGISTRY_PROXY_PORT", "50000") };
		let config = Config::default();
		assert!(config.validate().is_err());
		match original_port {
			Some(value) => unsafe { std::env::set_var("SVAMM_REGISTRY_PORT", value) },
			None => unsafe { std::env::remove_var("SVAMM_REGISTRY_PORT") },
		}
		match original_proxy {
			Some(value) => unsafe { std::env::set_var("SVAMM_REGISTRY_PROXY_PORT", value) },
			None => unsafe { std::env::remove_var("SVAMM_REGISTRY_PROXY_PORT") },
		}
	}

	#[test]
	fn parse_network_strict_absent_returns_none() {
		assert_eq!(
			parse_network_strict("trust_host_cas: false\nmax_connections: 256\n"),
			Ok(None)
		);
		assert_eq!(
			parse_network_strict("# tls:\n#     enabled: true\nstrict: false\n"),
			Ok(Some(false))
		);
	}

	#[test]
	fn parse_network_strict_accepts_booleans_and_comments() {
		assert_eq!(parse_network_strict("strict: false\n"), Ok(Some(false)));
		assert_eq!(
			parse_network_strict("strict:   true  # enforce\n"),
			Ok(Some(true))
		);
		assert_eq!(parse_network_strict("strict: \"false\"\n"), Ok(Some(false)));
	}

	#[test]
	fn parse_network_strict_rejects_invalid_values() {
		assert!(
			parse_network_strict("strict: on\n")
				.unwrap_err()
				.contains("expected true or false")
		);
		assert!(
			parse_network_strict("strict: maybe\n")
				.unwrap_err()
				.contains("expected true or false")
		);
	}

	#[test]
	fn network_strict_defaults_to_false_when_unset() {
		let dir = tempfile::tempdir().unwrap();
		let mut config = Config::default();
		config.configs.network = dir.path().join("network.yml").display().to_string();
		std::fs::write(&config.configs.network, "trust_host_cas: false\n").unwrap();
		assert!(!network_strict(&config).unwrap());
	}

	#[test]
	fn network_strict_honors_explicit_values() {
		let dir = tempfile::tempdir().unwrap();
		let mut config = Config::default();
		config.configs.network = dir.path().join("network.yml").display().to_string();
		std::fs::write(&config.configs.network, "strict: true\n").unwrap();
		assert!(network_strict(&config).unwrap());
	}

	#[test]
	fn network_strict_falls_back_to_embedded_default_when_file_missing() {
		let dir = tempfile::tempdir().unwrap();
		let mut config = Config::default();
		config.configs.network = dir.path().join("missing.yml").display().to_string();
		assert!(
			!network_strict(&config).unwrap(),
			"embedded default must be non-strict"
		);
	}
}
