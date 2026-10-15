//! svamm-owned per-sandbox state, stored outside the msb config.
//!
//! msb rewrites a sandbox's persisted config from its typed struct and drops
//! unknown fields. svamm stores the data that must survive such rewrites in
//! its own state directory instead of inside the msb config.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths;

/// the create-time mount inputs and the guests svamm claims as owned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountReplay {
	/// cli-provided specs (main mount + `--rw`/`--ro`) at create.
	#[serde(default)]
	pub cli: Vec<String>,
	/// guest tmpfs destination.
	#[serde(default)]
	pub tmpfs_dest: String,
	/// tmpfs size at create.
	#[serde(default)]
	pub tmpfs_size: String,
	/// effective guest-write quota applied to project rw mounts at create.
	///
	/// empty means the state predates the field and the configured default applies.
	#[serde(default)]
	pub workspace_quota: String,
	/// guest paths the sandbox's mounts claim as svamm-owned.
	#[serde(default)]
	pub applied: Vec<String>,
}

/// svamm-owned per-sandbox state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxState {
	/// create-time replay inputs for mount reconciliation.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub mounts: Option<MountReplay>,
	/// env keys svamm previously applied for the project.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub env_managed: Vec<String>,
}

/// the state file for one sandbox.
fn path(name: &str) -> PathBuf {
	paths::state_dir()
		.join("state")
		.join(format!("{name}.json"))
}

/// load a sandbox's state, or `None` when none was recorded.
pub fn load(name: &str) -> anyhow::Result<Option<SandboxState>> {
	let path = path(name);
	match std::fs::read_to_string(&path) {
		Ok(text) => serde_json::from_str(&text)
			.map(Some)
			.map_err(|err| anyhow::anyhow!("failed to parse {}: {err}", path.display())),
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
		Err(err) => anyhow::bail!("failed to read {}: {err}", path.display()),
	}
}

/// persist a sandbox's state atomically.
///
/// the state may hold resolved secret env values, so the file is written
/// owner-readable only.
pub fn save(name: &str, state: &SandboxState) -> anyhow::Result<()> {
	let path = path(name);
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent)?;
	}
	let text = serde_json::to_string(state)?;
	let tmp = path.with_extension("json.tmp");
	std::fs::write(&tmp, text)?;
	std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
	std::fs::rename(&tmp, &path)?;
	Ok(())
}

/// drop a sandbox's state. missing files are fine.
pub fn remove(name: &str) -> anyhow::Result<()> {
	match std::fs::remove_file(path(name)) {
		Ok(()) => Ok(()),
		Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
		Err(err) => Err(err.into()),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// serializes tests that mutate the process env.
	static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

	#[test]
	fn state_round_trips_in_state_dir() {
		let _guard = ENV_LOCK.lock().unwrap();
		let dir = tempfile::tempdir().unwrap();
		let original = std::env::var_os("SVAMM_STATE_DIR");
		unsafe { std::env::set_var("SVAMM_STATE_DIR", dir.path()) };
		assert_eq!(load("alpha").unwrap(), None);
		let state = SandboxState {
			mounts: Some(MountReplay {
				cli: vec!["/a:/home/agent/a:rw,quota=32G".to_string()],
				tmpfs_dest: "/tmp".to_string(),
				tmpfs_size: "2G".to_string(),
				workspace_quota: "32G".to_string(),
				applied: vec!["/tmp".to_string()],
			}),
			env_managed: vec!["SECRET".to_string()],
		};
		save("alpha", &state).unwrap();
		assert_eq!(load("alpha").unwrap(), Some(state.clone()));
		remove("alpha").unwrap();
		assert_eq!(load("alpha").unwrap(), None);
		match original {
			Some(value) => unsafe { std::env::set_var("SVAMM_STATE_DIR", value) },
			None => unsafe { std::env::remove_var("SVAMM_STATE_DIR") },
		}
	}
}
