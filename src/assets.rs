//! embedded default msb-facing configuration files.

use std::path::Path;

use crate::config::Config;
use crate::ui;

const DEFAULT_BASE: &str = include_str!("../configs/base.yml");
const DEFAULT_RESOURCES: &str = include_str!("../configs/resources.yml");
const DEFAULT_NETWORK: &str = include_str!("../configs/network.yml");
const DEFAULT_NET_RULES: &str = include_str!("../configs/net-rules.json");
const DEFAULT_REGISTRY_RESOURCES: &str = include_str!("../configs/registry-resources.yml");

/// the embedded default content for a named msb-facing config file.
pub fn embedded(name: &str) -> Option<&'static str> {
	match name {
		"base.yml" => Some(DEFAULT_BASE),
		"resources.yml" => Some(DEFAULT_RESOURCES),
		"network.yml" => Some(DEFAULT_NETWORK),
		"net-rules.json" => Some(DEFAULT_NET_RULES),
		"registry-resources.yml" => Some(DEFAULT_REGISTRY_RESOURCES),
		_ => None,
	}
}

/// write a named embedded config file to `path` when it is missing.
pub fn ensure_one(name: &str, path: &Path) {
	if path.exists() {
		return;
	}
	let Some(content) = embedded(name) else {
		return;
	};
	if let Some(parent) = path.parent() {
		if let Err(err) = std::fs::create_dir_all(parent) {
			ui::warn(format!(
				"failed to create config directory {}: {err}",
				parent.display()
			));
		}
	}
	if let Err(err) = std::fs::write(path, content) {
		ui::warn(format!(
			"failed to write default config {}: {err}",
			path.display()
		));
	}
}

/// create every default config file referenced by `config` that is missing on disk.
pub fn ensure_default_configs(config: &Config) {
	let paths = [
		("base.yml", &config.configs.base),
		("resources.yml", &config.configs.resources),
		("network.yml", &config.configs.network),
		("net-rules.json", &config.configs.net_rules),
		("registry-resources.yml", &config.configs.registry_resources),
	];
	for (name, path) in paths {
		let path = crate::paths::expand_tilde(path);
		ensure_one(name, &path);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn embedded_defaults_are_non_empty() {
		for name in [
			"base.yml",
			"resources.yml",
			"network.yml",
			"net-rules.json",
			"registry-resources.yml",
		] {
			assert!(
				embedded(name).is_some_and(|content| !content.trim().is_empty()),
				"{name} must have embedded content"
			);
		}
	}
}
