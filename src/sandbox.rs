//! sandbox query and introspection helpers.
//!
//! list svamm sandboxes, resolve handles by name, and extract information from
//! a sandbox's persisted config JSON.

use microsandbox::SandboxListBuilder;
use microsandbox::sandbox::{Sandbox, SandboxHandle, SandboxStatus};

use crate::config;

/// maximum sandbox list page size accepted by the SDK.
const MAX_LIST_LIMIT: u32 = 100;

/// fetch every sandbox, optionally filtered by a label.
pub async fn list_all(label: Option<(&str, &str)>) -> anyhow::Result<Vec<SandboxHandle>> {
	let mut handles = Vec::new();
	let mut cursor: Option<String> = None;
	loop {
		let page = Sandbox::list_with(|list: SandboxListBuilder| {
			let list = list.limit(MAX_LIST_LIMIT);
			let list = match label {
				Some((key, value)) => list.label(key, value),
				None => list,
			};
			match cursor.as_ref() {
				Some(next) => list.cursor(next.clone()),
				None => list,
			}
		})
		.await?;
		let len = page.sandboxes.len();
		handles.extend(page.sandboxes);
		match page.next_cursor {
			Some(next) => cursor = Some(next),
			None => break,
		}
		if len == 0 {
			break;
		}
	}
	Ok(handles)
}

/// true when a sandbox with `name` exists.
pub async fn sandbox_exists(name: &str) -> anyhow::Result<bool> {
	Ok(find_sandbox(name).await?.is_some())
}

/// true when a sandbox with `name` is running.
pub async fn sandbox_running(name: &str) -> anyhow::Result<bool> {
	Ok(find_sandbox(name)
		.await?
		.is_some_and(|s| s.status_snapshot() == SandboxStatus::Running))
}

/// find a sandbox handle by name.
pub async fn find_sandbox(name: &str) -> anyhow::Result<Option<SandboxHandle>> {
	match Sandbox::get(name).await {
		Ok(handle) => Ok(Some(handle)),
		Err(microsandbox::MicrosandboxError::SandboxNotFound(_)) => Ok(None),
		Err(err) => Err(err.into()),
	}
}

/// find a sandbox handle by name, with the standard error on failure.
pub async fn find_sandbox_or_err(name: &str) -> anyhow::Result<SandboxHandle> {
	find_sandbox(name).await?.ok_or_else(|| {
		anyhow::anyhow!(
			"sandbox '{name}' does not exist (create it first: 'svamm create {name} ...')"
		)
	})
}

/// extract OCI image info from a sandbox's persisted config JSON.
/// returns `(reference, manifest_digest, root_disk_kind)` for flat OCI sandboxes.
pub fn sandbox_oci_info(config_json: &str) -> (String, String, String) {
	let value: serde_json::Value = match serde_json::from_str(config_json) {
		Ok(value) => value,
		Err(_) => return (String::new(), String::new(), String::new()),
	};
	let image = value
		.get("image")
		.and_then(|image| image.get("Oci"))
		.or_else(|| value.get("image").and_then(|image| image.get("oci")));
	let Some(image) = image else {
		return (String::new(), String::new(), String::new());
	};
	let reference = image
		.get("reference")
		.and_then(|value| value.as_str())
		.unwrap_or("")
		.to_string();
	let digest = value
		.get("manifest_digest")
		.and_then(|value| value.as_str())
		.unwrap_or("")
		.to_string();
	let kind = image
		.get("root_disk")
		.and_then(|disk| disk.get("kind"))
		.and_then(|value| value.as_str())
		.unwrap_or("")
		.to_string();
	if reference.is_empty() || digest.is_empty() {
		(String::new(), String::new(), String::new())
	} else {
		(reference, digest, kind)
	}
}

/// the workdir from a sandbox's persisted config, if any.
pub fn workdir_from_handle(handle: &SandboxHandle) -> Option<String> {
	let config = handle.config().ok()?;
	let workdir = config.spec.runtime.workdir.as_deref().unwrap_or("");
	if workdir.is_empty() {
		None
	} else {
		Some(workdir.to_string())
	}
}

/// the label selector for svamm-managed sandboxes.
pub fn managed_selector() -> (&'static str, &'static str) {
	(config::MANAGED_LABEL_KEY, config::MANAGED_LABEL_VALUE)
}

/// true when the sandbox's persisted config has the `svamm` label key.
/// the SDK filters only by exact `key=value` pairs, so callers list all and filter here.
pub fn is_svamm_sandbox(config_json: &str) -> bool {
	let Ok(value) = serde_json::from_str::<serde_json::Value>(config_json) else {
		return false;
	};
	value
		.get("labels")
		.and_then(|labels| labels.as_object())
		.is_some_and(|labels| labels.contains_key(config::MANAGED_LABEL_KEY))
}
