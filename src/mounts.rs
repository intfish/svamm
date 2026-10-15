//! mount specification helpers.
//!
//! these helpers absolutize host paths, derive guest destinations, build
//! `host:dest:mode` specs, and detect destination conflicts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// a parsed `host:dest:mode` mount specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountSpec {
	/// absolute host source path.
	pub source: String,
	/// guest destination path.
	pub dest: String,
	/// mount mode (`rw` or `ro`).
	pub mode: String,
}

/// convert a host path to an absolute path.
///
/// a leading `~` expands to the user's home directory. empty input resolves
/// to the current directory. absolute paths pass through.
pub fn abs_path(path: &str) -> String {
	let expanded = crate::paths::expand_tilde(path)
		.to_string_lossy()
		.into_owned();
	if expanded.is_empty() {
		std::env::current_dir()
			.map(|dir| dir.display().to_string())
			.unwrap_or_else(|_| ".".to_string())
	} else if expanded.starts_with('/') {
		expanded.clone()
	} else {
		let cwd = std::env::current_dir()
			.map(|dir| dir.display().to_string())
			.unwrap_or_else(|_| ".".to_string());
		format!("{cwd}/{expanded}")
	}
}

/// guest mount destination for a host path.
///
/// the destination is `/home/agent/<basename>`, or the agent home itself when
/// the basename is empty, `/`, or `.`.
pub fn dest_for_host(host: &str, agent_home: &str) -> String {
	let base = Path::new(host)
		.file_name()
		.map(|name| name.to_string_lossy().into_owned())
		.unwrap_or_default();
	if base.is_empty() || base == "/" || base == "." {
		agent_home.to_string()
	} else {
		format!("{agent_home}/{base}")
	}
}

/// build a `host:dest:mode` mount spec for a host path.
pub fn mount_spec(host: &str, mode: &str, agent_home: &str) -> MountSpec {
	let abs = abs_path(host);
	let dest = dest_for_host(&abs, agent_home);
	MountSpec {
		source: abs,
		dest,
		mode: mode.to_string(),
	}
}

/// parse a `host:dest:mode` spec into parts.
pub fn parse_spec(spec: &str) -> (&str, &str, &str) {
	let mut parts = spec.splitn(3, ':');
	let source = parts.next().unwrap_or("");
	let dest = parts.next().unwrap_or("");
	let mode = parts.next().unwrap_or("rw");
	(source, dest, mode)
}

impl MountSpec {
	pub fn from_spec(spec: &str) -> MountSpec {
		let (source, dest, mode) = parse_spec(spec);
		MountSpec {
			source: source.to_string(),
			dest: dest.to_string(),
			mode: mode.to_string(),
		}
	}
}

/// reject duplicate mount destinations.
///
/// entries are `source:dest:mode`. the source may be a label like `[tmpfs]`.
pub fn check_mount_conflicts(specs: &[String]) -> Result<(), String> {
	let mut seen: HashMap<&str, &str> = HashMap::new();
	for spec in specs {
		let (source, dest, _mode) = parse_spec(spec);
		if let Some(previous) = seen.get(dest) {
			return Err(format!(
				"mount destination conflict: '{dest}' (from '{previous}' and '{source}')"
			));
		}
		seen.insert(dest, source);
	}
	Ok(())
}

/// classify a mount source as `dir`, `file`, or `missing`.
pub fn mount_kind(source: &str) -> &'static str {
	let path = Path::new(source);
	if path.is_dir() {
		"dir"
	} else if path.is_file() {
		"file"
	} else {
		"missing"
	}
}

/// locate `<main>/svamm.json` for a main mount directory, if present.
pub fn project_config(main_abs: &str) -> Option<PathBuf> {
	let dir = Path::new(main_abs);
	if dir.is_dir() {
		let candidate = dir.join("svamm.json");
		if candidate.is_file() {
			return Some(candidate);
		}
	}
	None
}

/// a single mount declared in a project `svamm.json`.
///
/// every entry is an object with a required `src` and `mode`. the guest
/// destination is derived from `src` when `dest` is unset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectMount {
	/// host source path.
	pub src: String,
	/// guest destination. derived from `src` when unset.
	pub dest: Option<String>,
	/// mount mode (`rw` or `ro`).
	pub mode: String,
}

/// read the `mounts` array from a project `svamm.json`, if present.
///
/// `"mounts"` declares additional mounts merged with the `--rw`/`--ro` create
/// flags. each entry is `{"src", "dest"?, "mode"}`; `"src"` and `"mode"` are
/// required. rw entries get the same guest-write quota as flag-provided mounts.
pub fn project_mounts(path: &Path) -> Result<Option<Vec<ProjectMount>>, String> {
	let text = std::fs::read_to_string(path)
		.map_err(|err| format!("failed to read {}: {err}", path.display()))?;
	let data: Value = serde_json::from_str(&text)
		.map_err(|err| format!("invalid JSON in {}: {err}", path.display()))?;
	let Some(mounts) = data.get("mounts") else {
		return Ok(None);
	};
	let Some(mounts) = mounts.as_array() else {
		return Err(format!(
			"{}: \"mounts\" must be a list of {{\"src\", \"dest\"?, \"mode\"}} objects",
			path.display()
		));
	};
	if mounts.is_empty() {
		return Ok(Some(Vec::new()));
	}
	Ok(Some(
		mounts
			.iter()
			.enumerate()
			.map(|(index, entry)| parse_mount_entry(path, index, entry))
			.collect::<Result<Vec<_>, _>>()?,
	))
}

fn parse_mount_entry(path: &Path, index: usize, entry: &Value) -> Result<ProjectMount, String> {
	let where_ = format!("{}.mounts[{index}]", path.display());
	let Some(object) = entry.as_object() else {
		return Err(format!(
			"{where_}: must be an object with \"src\", \"dest\"?, \"mode\""
		));
	};
	let src = object
		.get("src")
		.and_then(Value::as_str)
		.map(str::trim)
		.filter(|src| !src.is_empty());
	let Some(src) = src else {
		return Err(format!("{where_}: requires a non-empty \"src\" string"));
	};
	let dest = object
		.get("dest")
		.and_then(Value::as_str)
		.map(str::trim)
		.filter(|dest| !dest.is_empty())
		.map(str::to_string);
	let mode = object
		.get("mode")
		.and_then(Value::as_str)
		.map(str::trim)
		.filter(|mode| !mode.is_empty());
	let Some(mode) = mode else {
		return Err(format!(
			"{where_}: requires a non-empty \"mode\" (\"rw\" or \"ro\")"
		));
	};
	if mode != "rw" && mode != "ro" {
		return Err(format!(
			"{where_}: \"mode\" must be \"rw\" or \"ro\", got \"{mode}\""
		));
	}
	Ok(ProjectMount {
		src: src.to_string(),
		dest,
		mode: mode.to_string(),
	})
}

/// build a `host:dest:mode` mount spec for a project-declared mount.
///
/// rw entries carry the same guest-write quota as flag-provided mounts.
pub fn project_mount_spec(mount: &ProjectMount, quota: &str, agent_home: &str) -> String {
	let source = abs_path(&mount.src);
	let dest = match &mount.dest {
		Some(dest) => dest.clone(),
		None => dest_for_host(&source, agent_home),
	};
	let spec = format!("{source}:{dest}:{}", mount.mode);
	if mount.mode == "rw" {
		format!("{spec},quota={quota}")
	} else {
		spec
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// serialize tests that mutate the process env.
	static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

	#[test]
	fn abs_path_behavior() {
		assert_eq!(abs_path("/abs/path"), "/abs/path");
		let cwd = std::env::current_dir().unwrap();
		assert_eq!(abs_path("rel/path"), format!("{}/rel/path", cwd.display()));
		assert_eq!(abs_path(""), cwd.display().to_string());
	}

	#[test]
	fn abs_path_expands_tilde() {
		let _guard = ENV_LOCK.lock().unwrap();
		let home = tempfile::tempdir().unwrap();
		let original = std::env::var_os("HOME");
		unsafe { std::env::set_var("HOME", home.path()) };
		assert_eq!(
			abs_path("~/ref"),
			home.path().join("ref").display().to_string()
		);
		match original {
			Some(value) => unsafe { std::env::set_var("HOME", value) },
			None => unsafe { std::env::remove_var("HOME") },
		}
	}

	#[test]
	fn dest_for_host_behavior() {
		let home = "/home/agent";
		assert_eq!(dest_for_host("/a/b/c", home), "/home/agent/c");
		assert_eq!(dest_for_host("/a/b/", home), "/home/agent/b");
		assert_eq!(dest_for_host("/", home), "/home/agent");
		assert_eq!(dest_for_host(".", home), "/home/agent");
	}

	#[test]
	fn mount_spec_construction() {
		let spec = mount_spec("/home/me/project", "rw", "/home/agent");
		assert_eq!(spec.source, "/home/me/project");
		assert_eq!(spec.dest, "/home/agent/project");
		assert_eq!(spec.mode, "rw");
	}

	#[test]
	fn parse_spec_split() {
		let (source, dest, mode) = parse_spec("/a/b:/home/agent/b:rw");
		assert_eq!(source, "/a/b");
		assert_eq!(dest, "/home/agent/b");
		assert_eq!(mode, "rw");
	}

	#[test]
	fn conflict_detection_happy_path() {
		let specs = vec![
			"/ref/src:/home/agent/ref:ro".to_string(),
			"[tmpfs]:/tmp:rw".to_string(),
			"/a/b:/home/agent/b:rw".to_string(),
		];
		check_mount_conflicts(&specs).unwrap();
	}

	#[test]
	fn conflict_detection_rejects_duplicates() {
		let err = check_mount_conflicts(&[
			"/a/dup:/home/agent/dup:rw".to_string(),
			"/b/dup:/home/agent/dup:ro".to_string(),
		])
		.unwrap_err();
		assert!(err.contains("mount destination conflict: '/home/agent/dup'"));
	}

	#[test]
	fn mount_kind_classification() {
		let dir = tempfile::tempdir().unwrap();
		let file = tempfile::NamedTempFile::new().unwrap();
		assert_eq!(mount_kind(dir.path().to_str().unwrap()), "dir");
		assert_eq!(mount_kind(file.path().to_str().unwrap()), "file");
		assert_eq!(mount_kind("/nope/does/not/exist"), "missing");
	}

	fn write_json(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
		let path = dir.join(name);
		std::fs::write(&path, content).unwrap();
		path
	}

	#[test]
	fn project_mounts_absent_returns_none() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(dir.path(), "svamm.json", r#"{"network": {"allow": []}}"#);
		assert_eq!(project_mounts(&file).unwrap(), None);
	}

	#[test]
	fn project_mounts_parses_objects() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(
			dir.path(),
			"svamm.json",
			r#"{"mounts": [
				{"src": "/srv/work", "dest": "/work", "mode": "rw"},
				{"src": "/srv/ref", "mode": "ro"}
			]}"#,
		);
		let mounts = project_mounts(&file).unwrap().unwrap();
		assert_eq!(
			mounts,
			vec![
				ProjectMount {
					src: "/srv/work".to_string(),
					dest: Some("/work".to_string()),
					mode: "rw".to_string(),
				},
				ProjectMount {
					src: "/srv/ref".to_string(),
					dest: None,
					mode: "ro".to_string(),
				},
			]
		);
	}

	#[test]
	fn project_mounts_empty_list_is_no_mounts() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(dir.path(), "svamm.json", r#"{"mounts": []}"#);
		assert_eq!(project_mounts(&file).unwrap(), Some(vec![]));
	}

	#[test]
	fn project_mounts_rejects_non_array() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(
			dir.path(),
			"svamm.json",
			r#"{"mounts": {"rw": ["/srv/work"]}}"#,
		);
		let err = project_mounts(&file).unwrap_err();
		assert!(
			err.contains("\"mounts\" must be a list"),
			"unexpected error: {err}"
		);
	}

	#[test]
	fn project_mounts_rejects_string_entry() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(dir.path(), "svamm.json", r#"{"mounts": ["/srv/work"]}"#);
		let err = project_mounts(&file).unwrap_err();
		assert!(err.contains("must be an object"), "unexpected error: {err}");
	}

	#[test]
	fn project_mounts_rejects_object_without_src() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(
			dir.path(),
			"svamm.json",
			r#"{"mounts": [{"dest": "/home/agent/ref", "mode": "ro"}]}"#,
		);
		let err = project_mounts(&file).unwrap_err();
		assert!(
			err.contains("requires a non-empty \"src\""),
			"unexpected error: {err}"
		);
	}

	#[test]
	fn project_mounts_rejects_object_without_mode() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(
			dir.path(),
			"svamm.json",
			r#"{"mounts": [{"src": "/srv/ref"}]}"#,
		);
		let err = project_mounts(&file).unwrap_err();
		assert!(
			err.contains("requires a non-empty \"mode\""),
			"unexpected error: {err}"
		);
	}

	#[test]
	fn project_mounts_rejects_bad_mode() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(
			dir.path(),
			"svamm.json",
			r#"{"mounts": [{"src": "/srv/ref", "mode": "rwe"}]}"#,
		);
		let err = project_mounts(&file).unwrap_err();
		assert!(
			err.contains("\"mode\" must be \"rw\" or \"ro\", got \"rwe\""),
			"unexpected error: {err}"
		);
	}

	#[test]
	fn project_mounts_rejects_bad_json() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(dir.path(), "svamm.json", "not json");
		assert!(project_mounts(&file).unwrap_err().contains("invalid JSON"));
	}
}
