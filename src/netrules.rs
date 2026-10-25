//! network rules merging (base net-rules.json + project svamm.json).
//!
//! base rules stay first, project rules are appended with duplicates removed,
//! and only non-empty `allow`/`deny` lists are produced.

use std::path::Path;

use serde_json::Value;

/// merged allow/deny rule lists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MergedRules {
	/// allowed destinations (`host:tcp:port` tokens).
	pub allow: Vec<String>,
	/// denied destinations.
	pub deny: Vec<String>,
}

impl MergedRules {
	/// convert to msb `--net-rule` tokens (`deny@<entry>`, `allow@<entry>`).
	///
	/// deny entries come first. msb evaluates rules first-match-wins, so a deny
	/// can then override a broader allow.
	pub fn to_tokens(&self) -> Vec<String> {
		let mut tokens = Vec::new();
		tokens.extend(self.deny.iter().map(|entry| format!("deny@{entry}")));
		tokens.extend(self.allow.iter().map(|entry| format!("allow@{entry}")));
		tokens
	}
}

/// merge the base net-rules file with an optional project file.
///
/// project rules are appended after base rules with duplicates removed. only
/// `allow`/`deny` are used; other keys under `network` are ignored.
pub fn merge(base: &Path, project: Option<&Path>) -> Result<MergedRules, String> {
	let mut merged = load(base)?;
	if let Some(project) = project {
		let project_rules = load(project)?;
		for value in project_rules.allow {
			if !merged.allow.contains(&value) {
				merged.allow.push(value);
			}
		}
		for value in project_rules.deny {
			if !merged.deny.contains(&value) {
				merged.deny.push(value);
			}
		}
	}
	Ok(merged)
}

/// load `allow`/`deny` lists from a rules file.
///
/// a missing `network` object is an empty rule set, so project files may
/// configure other features without a network section.
fn load(path: &Path) -> Result<MergedRules, String> {
	let text = std::fs::read_to_string(path)
		.map_err(|err| format!("failed to read {}: {err}", path.display()))?;
	let data: Value = serde_json::from_str(&text)
		.map_err(|err| format!("invalid JSON in {}: {err}", path.display()))?;
	if !data.is_object() {
		return Err(format!(
			"{}: expected a JSON object with a \"network\" object",
			path.display()
		));
	}
	let Some(network) = data.as_object().and_then(|object| object.get("network")) else {
		return Ok(MergedRules::default());
	};
	let Some(network) = network.as_object() else {
		return Err(format!(
			"{}: expected an object with a \"network\" object",
			path.display()
		));
	};
	let mut rules = MergedRules::default();
	for key in ["allow", "deny"] {
		let value = network
			.get(key)
			.cloned()
			.unwrap_or_else(|| Value::Array(vec![]));
		let Value::Array(values) = value else {
			return Err(format!(
				"{}: network.{key} must be a list of strings",
				path.display()
			));
		};
		if !values.iter().all(|entry| entry.is_string()) {
			return Err(format!(
				"{}: network.{key} must be a list of strings",
				path.display()
			));
		}
		let entries: Vec<String> = values
			.into_iter()
			.filter_map(|entry| entry.as_str().map(str::to_string))
			.collect();
		if key == "allow" {
			rules.allow = entries;
		} else {
			rules.deny = entries;
		}
	}
	Ok(rules)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn write_json(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
		let path = dir.join(name);
		std::fs::write(&path, content).unwrap();
		path
	}

	#[test]
	fn base_only_output_shape_and_order() {
		let dir = tempfile::tempdir().unwrap();
		let base = write_json(
			dir.path(),
			"base.json",
			r#"{"network": {"allow": ["a.com:tcp:443", "b.com:tcp:443"], "deny": ["x.net"]}}"#,
		);
		let merged = merge(&base, None).unwrap();
		assert_eq!(merged.allow, vec!["a.com:tcp:443", "b.com:tcp:443"]);
		assert_eq!(merged.deny, vec!["x.net"]);
	}

	#[test]
	fn project_rules_append_after_base_deduplicated() {
		let dir = tempfile::tempdir().unwrap();
		let base = write_json(
			dir.path(),
			"base.json",
			r#"{"network": {"allow": ["a.com:tcp:443", "b.com:tcp:443"], "deny": ["x.net"]}}"#,
		);
		let project = write_json(
			dir.path(),
			"proj.json",
			r#"{"network": {"allow": ["b.com:tcp:443", "c.com:tcp:443"], "deny": ["y.net"]}}"#,
		);
		let merged = merge(&base, Some(&project)).unwrap();
		assert_eq!(
			merged.allow,
			vec!["a.com:tcp:443", "b.com:tcp:443", "c.com:tcp:443"]
		);
		assert_eq!(merged.deny, vec!["x.net", "y.net"]);
	}

	#[test]
	fn empty_project_rules_keep_base_list() {
		let dir = tempfile::tempdir().unwrap();
		let base = write_json(
			dir.path(),
			"base.json",
			r#"{"network": {"allow": ["a.com:tcp:443"], "deny": ["x.net"]}}"#,
		);
		let project = write_json(
			dir.path(),
			"proj.json",
			r#"{"network": {"allow": [], "deny": []}}"#,
		);
		let merged = merge(&base, Some(&project)).unwrap();
		assert_eq!(merged.allow, vec!["a.com:tcp:443"]);
		assert_eq!(merged.deny, vec!["x.net"]);
	}

	#[test]
	fn ports_only_project_file_keeps_base_rules() {
		let dir = tempfile::tempdir().unwrap();
		let base = write_json(
			dir.path(),
			"base.json",
			r#"{"network": {"allow": ["a.com:tcp:443"], "deny": ["x.net"]}}"#,
		);
		let project = write_json(dir.path(), "proj.json", r#"{"ports": ["9000:80"]}"#);
		let merged = merge(&base, Some(&project)).unwrap();
		assert_eq!(merged.allow, vec!["a.com:tcp:443"]);
		assert_eq!(merged.deny, vec!["x.net"]);
	}

	#[test]
	fn unknown_network_keys_are_ignored() {
		let dir = tempfile::tempdir().unwrap();
		let base = write_json(
			dir.path(),
			"base.json",
			r#"{"network": {"allow": ["a.com:tcp:443"], "tls": {"enabled": true}, "future": 1}}"#,
		);
		let merged = merge(&base, None).unwrap();
		assert_eq!(merged.allow, vec!["a.com:tcp:443"]);
		assert!(merged.deny.is_empty());
	}

	#[test]
	fn to_tokens_maps_deny_before_allow() {
		let rules = MergedRules {
			allow: vec!["a.com:tcp:443".to_string()],
			deny: vec!["x.net".to_string()],
		};
		assert_eq!(rules.to_tokens(), vec!["deny@x.net", "allow@a.com:tcp:443"]);
	}

	#[test]
	fn to_tokens_omits_empty_lists() {
		let rules = MergedRules::default();
		assert!(rules.to_tokens().is_empty());

		let only_allow = MergedRules {
			allow: vec!["a.com:tcp:443".to_string()],
			deny: vec![],
		};
		assert_eq!(only_allow.to_tokens(), vec!["allow@a.com:tcp:443"]);
	}

	#[test]
	fn rejects_invalid_input() {
		let dir = tempfile::tempdir().unwrap();
		let bad = write_json(dir.path(), "bad.json", "not json");
		assert!(merge(&bad, None).unwrap_err().contains("invalid JSON"));

		let nowrap = write_json(dir.path(), "nowrap.json", r#"{"foo": 1}"#);
		assert_eq!(merge(&nowrap, None).unwrap(), MergedRules::default());

		let badnetwork = write_json(
			dir.path(),
			"badnetwork.json",
			r#"{"network": "not-an-object"}"#,
		);
		assert!(
			merge(&badnetwork, None)
				.unwrap_err()
				.contains("expected an object with a \"network\" object")
		);

		let badallow = write_json(
			dir.path(),
			"badallow.json",
			r#"{"network": {"allow": "x"}}"#,
		);
		assert!(
			merge(&badallow, None)
				.unwrap_err()
				.contains("network.allow must be a list of strings")
		);

		let baddeny = write_json(
			dir.path(),
			"baddeny.json",
			r#"{"network": {"deny": [1, 2]}}"#,
		);
		assert!(
			merge(&baddeny, None)
				.unwrap_err()
				.contains("network.deny must be a list of strings")
		);
	}
}
