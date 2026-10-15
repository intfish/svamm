//! XDG path resolution and environment overrides.
//!
//! svamm stores configuration under the XDG config directory and mutable state
//! under the XDG state directory. every path has an environment variable override.

use std::path::PathBuf;

/// the svamm configuration directory (`~/.config/svamm/`).
///
/// `SVAMM_CONFIG_DIR` takes precedence when set. otherwise use the XDG config
/// directory, falling back to `~/.config/svamm`.
pub fn config_dir() -> PathBuf {
	if let Ok(value) = std::env::var("SVAMM_CONFIG_DIR") {
		if !value.is_empty() {
			return PathBuf::from(value);
		}
	}
	dirs::config_dir()
		.unwrap_or_else(|| home_dir().join(".config"))
		.join("svamm")
}

/// the svamm state directory (`~/.local/state/svamm/`).
///
/// `SVAMM_STATE_DIR` takes precedence when set. otherwise use the XDG state
/// directory, falling back to `~/.local/state/svamm`.
pub fn state_dir() -> PathBuf {
	if let Ok(value) = std::env::var("SVAMM_STATE_DIR") {
		if !value.is_empty() {
			return PathBuf::from(value);
		}
	}
	dirs::state_dir()
		.unwrap_or_else(|| home_dir().join(".local/state"))
		.join("svamm")
}

/// the microsandbox home directory.
///
/// a non-empty `MSB_HOME` wins, otherwise `~/.microsandbox`.
pub fn msb_home() -> PathBuf {
	if let Some(value) = std::env::var_os("MSB_HOME") {
		if !value.is_empty() {
			return PathBuf::from(value);
		}
	}
	home_dir().join(".microsandbox")
}

/// the user's home directory, or the current directory as a last resort.
pub fn home_dir() -> PathBuf {
	dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// the user binary directory for svamm command links.
///
/// `SVAMM_BIN_DIR` takes precedence when set. otherwise use `$XDG_BIN_HOME`,
/// then `~/.local/bin`.
pub fn bin_dir() -> PathBuf {
	if let Ok(value) = std::env::var("SVAMM_BIN_DIR") {
		if !value.is_empty() {
			return PathBuf::from(value);
		}
	}
	if let Ok(value) = std::env::var("XDG_BIN_HOME") {
		if !value.is_empty() {
			return PathBuf::from(value);
		}
	}
	home_dir().join(".local/bin")
}

/// expand a leading `~` to the user's home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
	if let Some(rest) = path.strip_prefix("~/") {
		home_dir().join(rest)
	} else if path == "~" {
		home_dir()
	} else {
		PathBuf::from(path)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn config_dir_honors_env_override() {
		let original = std::env::var_os("SVAMM_CONFIG_DIR");
		unsafe { std::env::set_var("SVAMM_CONFIG_DIR", "/tmp/svamm-cfg") };
		assert_eq!(config_dir(), PathBuf::from("/tmp/svamm-cfg"));
		match original {
			Some(value) => unsafe { std::env::set_var("SVAMM_CONFIG_DIR", value) },
			None => unsafe { std::env::remove_var("SVAMM_CONFIG_DIR") },
		}
	}

	#[test]
	fn msb_home_honors_env_override() {
		let original = std::env::var_os("MSB_HOME");
		unsafe { std::env::set_var("MSB_HOME", "/tmp/msb-home") };
		assert_eq!(msb_home(), PathBuf::from("/tmp/msb-home"));
		match original {
			Some(value) => unsafe { std::env::set_var("MSB_HOME", value) },
			None => unsafe { std::env::remove_var("MSB_HOME") },
		}
	}

	#[test]
	fn expand_tilde_resolves_home() {
		let home = home_dir();
		assert_eq!(expand_tilde("~/x/y"), home.join("x/y"));
		assert_eq!(expand_tilde("~"), home);
		assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
	}
}
