//! port publishing.
//!
//! svamm ports come from a project `svamm.json` `"network"."ports"` list,
//! using the same msb-compatible `HOST:GUEST` syntax.
use std::net::IpAddr;
use std::path::Path;

use serde_json::Value;

/// a parsed, validated host-to-guest port mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMapping {
	/// host address to bind (defaults to loopback).
	pub bind: IpAddr,
	/// host-side port to bind.
	pub host_port: u16,
	/// guest-side port to forward to.
	pub guest_port: u16,
	/// true when the mapping is UDP (false = TCP).
	pub udp: bool,
}

impl PortMapping {
	/// the host-side bind address and port, without the guest mapping.
	pub fn host_endpoint(&self) -> String {
		if self.bind == IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) {
			self.host_port.to_string()
		} else {
			format!("{}:{}", self.bind, self.host_port)
		}
	}

	/// the msb `--port` style string for this mapping.
	pub fn to_port_string(&self) -> String {
		let proto = if self.udp { "/udp" } else { "" };
		format!("{}:{}{}", self.host_endpoint(), self.guest_port, proto)
	}

	/// the typed persisted spec for this mapping.
	pub fn to_spec(&self) -> microsandbox_types::PublishedPortSpec {
		microsandbox_types::PublishedPortSpec {
			host_port: self.host_port,
			guest_port: self.guest_port,
			protocol: if self.udp {
				microsandbox_types::PortProtocol::Udp
			} else {
				microsandbox_types::PortProtocol::Tcp
			},
			host_bind: self.bind.to_string(),
		}
	}
}

/// a normalized mapping from a persisted port spec.
///
/// a missing or unparseable bind address falls back to loopback.
pub fn from_spec(spec: &microsandbox_types::PublishedPortSpec) -> PortMapping {
	PortMapping {
		bind: spec
			.host_bind
			.parse()
			.unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
		host_port: spec.host_port,
		guest_port: spec.guest_port,
		udp: spec.protocol == microsandbox_types::PortProtocol::Udp,
	}
}

/// parse an msb-compatible port mapping spec.
///
/// accepted forms are `HOST:GUEST`, `BIND_ADDR:HOST:GUEST`, and
/// `[ADDR]:HOST:GUEST`, each optionally suffixed with `/tcp` or `/udp`. the
/// bind address defaults to loopback when omitted.
pub fn parse_port(spec: &str) -> Result<PortMapping, String> {
	use std::net::Ipv4Addr;

	let (port_part, udp) = if let Some(rest) = spec.strip_suffix("/udp") {
		(rest, true)
	} else if let Some(rest) = spec.strip_suffix("/tcp") {
		(rest, false)
	} else {
		(spec, false)
	};

	let (bind, host_str, guest_str) = if let Some(rest) = port_part.strip_prefix('[') {
		let (bind_str, after_bracket) = rest.split_once("]:").ok_or_else(|| {
			format!("invalid port '{spec}': IPv6 bind must be [ADDR]:HOST:GUEST[/udp]")
		})?;
		let (host_str, guest_str) = after_bracket
			.split_once(':')
			.ok_or_else(|| format!("invalid port '{spec}': expected [ADDR]:HOST:GUEST[/udp]"))?;
		let bind = parse_ip(bind_str)
			.ok_or_else(|| format!("invalid port '{spec}': bad bind address '{bind_str}'"))?;
		(bind, host_str, guest_str)
	} else {
		let parts: Vec<&str> = port_part.split(':').collect();
		match parts.as_slice() {
			[host_str, guest_str] => (IpAddr::V4(Ipv4Addr::LOCALHOST), *host_str, *guest_str),
			[bind_str, host_str, guest_str] => {
				let bind = parse_ip(bind_str).ok_or_else(|| {
					format!("invalid port '{spec}': bad bind address '{bind_str}'")
				})?;
				(bind, *host_str, *guest_str)
			}
			_ => {
				return Err(format!(
					"invalid port '{spec}': expected HOST:GUEST[/udp] or BIND_ADDR:HOST:GUEST[/udp]"
				));
			}
		}
	};

	let host_port: u16 = host_str.trim().parse().map_err(|_| {
		format!("invalid port '{spec}': host port '{host_str}' is not a valid port")
	})?;
	let guest_port: u16 = guest_str.trim().parse().map_err(|_| {
		format!("invalid port '{spec}': guest port '{guest_str}' is not a valid port")
	})?;

	Ok(PortMapping {
		bind,
		host_port,
		guest_port,
		udp,
	})
}

/// read the `ports` list from a project `svamm.json`, if present.
///
/// ports come from the msb-compatible `network.ports` list. a top-level
/// `ports` list is also accepted and appended after `network.ports`. any
/// present key must be a list of msb-compatible port strings.
/// returns `Ok(None)` when the file has neither key.
pub fn project_ports(path: &Path) -> Result<Option<Vec<PortMapping>>, String> {
	let text = std::fs::read_to_string(path)
		.map_err(|err| format!("failed to read {}: {err}", path.display()))?;
	let data: Value = serde_json::from_str(&text)
		.map_err(|err| format!("invalid JSON in {}: {err}", path.display()))?;

	let mut raw: Vec<String> = Vec::new();
	let mut found = false;

	if let Some(network) = data.get("network") {
		let network = network.as_object().ok_or_else(|| {
			format!(
				"{}: expected an object with a \"network\" object",
				path.display()
			)
		})?;
		if let Some(ports) = network.get("ports") {
			found = true;
			raw.extend(ports_strings(ports, path, "network.ports")?);
		}
	}
	if let Some(ports) = data.get("ports") {
		found = true;
		raw.extend(ports_strings(ports, path, "ports")?);
	}
	if !found {
		return Ok(None);
	}

	let mut mappings = Vec::new();
	for spec in raw {
		let mapping = parse_port(&spec).map_err(|err| format!("{}: {err}", path.display()))?;
		mappings.push(mapping);
	}
	Ok(Some(mappings))
}

/// read a JSON list of port strings. fail loudly when it is not a list.
fn ports_strings(ports: &Value, path: &Path, key: &str) -> Result<Vec<String>, String> {
	let Some(values) = ports.as_array() else {
		return Err(format!(
			"{}: \"{key}\" must be a list of strings",
			path.display()
		));
	};
	let mut out = Vec::new();
	for value in values {
		let Some(spec) = value.as_str() else {
			return Err(format!(
				"{}: \"{key}\" must be a list of strings",
				path.display()
			));
		};
		out.push(spec.to_string());
	}
	Ok(out)
}

/// the published ports of a sandbox config, normalized.
pub fn from_config(config: &microsandbox::sandbox::SandboxConfig) -> Vec<PortMapping> {
	config.spec.network.ports.iter().map(from_spec).collect()
}

fn parse_ip(value: &str) -> Option<IpAddr> {
	value.trim().parse().ok()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parse_two_part_spec_defaults_to_loopback_tcp() {
		let mapping = parse_port("9000:80").unwrap();
		assert_eq!(mapping.bind, IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
		assert_eq!(mapping.host_port, 9000);
		assert_eq!(mapping.guest_port, 80);
		assert!(!mapping.udp);
	}

	#[test]
	fn parse_udp_and_tcp_suffixes() {
		assert!(parse_port("5353:53/udp").unwrap().udp);
		assert!(!parse_port("5353:53/tcp").unwrap().udp);
	}

	#[test]
	fn parse_bind_address_forms() {
		let mapping = parse_port("0.0.0.0:8080:80").unwrap();
		assert_eq!(mapping.bind, "0.0.0.0".parse::<IpAddr>().unwrap());
		assert_eq!(mapping.host_port, 8080);
		assert_eq!(mapping.guest_port, 80);

		let v6 = parse_port("[::1]:8081:81").unwrap();
		assert_eq!(v6.bind, "::1".parse::<IpAddr>().unwrap());
	}

	#[test]
	fn parse_rejects_invalid_specs() {
		assert!(
			parse_port("9000")
				.unwrap_err()
				.contains("expected HOST:GUEST")
		);
		assert!(parse_port("a:b").unwrap_err().contains("host port 'a'"));
		assert!(parse_port("9000:x").unwrap_err().contains("guest port 'x'"));
		assert!(
			parse_port("99999:80")
				.unwrap_err()
				.contains("host port '99999'")
		);
		assert!(
			parse_port("1.2.3.4.5:9000:80")
				.unwrap_err()
				.contains("bad bind address")
		);
		assert!(
			parse_port("[::1]9000:80")
				.unwrap_err()
				.contains("IPv6 bind")
		);
	}

	#[test]
	fn to_port_string_round_trips() {
		let mapping = parse_port("9000:80").unwrap();
		assert_eq!(mapping.to_port_string(), "9000:80");

		let udp = parse_port("127.0.0.1:5353:53/udp").unwrap();
		assert_eq!(udp.to_port_string(), "5353:53/udp");

		let bound = parse_port("0.0.0.0:8080:80").unwrap();
		assert_eq!(bound.to_port_string(), "0.0.0.0:8080:80");
	}

	#[test]
	fn host_endpoint_omits_loopback_bind() {
		let mapping = parse_port("9000:80").unwrap();
		assert_eq!(mapping.host_endpoint(), "9000");

		let bound = parse_port("0.0.0.0:8080:80").unwrap();
		assert_eq!(bound.host_endpoint(), "0.0.0.0:8080");

		let v6 = parse_port("[::1]:8081:81").unwrap();
		assert_eq!(v6.host_endpoint(), "::1:8081");
	}

	#[test]
	fn spec_round_trips_through_the_persisted_shape() {
		let mapping = parse_port("0.0.0.0:8080:80").unwrap();
		let spec = mapping.to_spec();
		assert_eq!(spec.host_port, 8080);
		assert_eq!(spec.guest_port, 80);
		assert_eq!(spec.host_bind, "0.0.0.0");
		assert_eq!(from_spec(&spec), mapping);
		assert_eq!(
			serde_json::to_value(&spec).unwrap(),
			serde_json::json!({
				"host_port": 8080,
				"guest_port": 80,
				"protocol": "tcp",
				"host_bind": "0.0.0.0",
			})
		);
	}

	#[test]
	fn from_spec_normalizes_missing_bind_to_loopback() {
		let mapping = from_spec(&microsandbox_types::PublishedPortSpec {
			host_port: 9000,
			guest_port: 80,
			protocol: microsandbox_types::PortProtocol::Tcp,
			host_bind: "not-an-ip".to_string(),
		});
		assert_eq!(mapping.to_port_string(), "9000:80");
	}

	#[test]
	fn published_ports_parses_persisted_config() {
		let config: microsandbox::sandbox::SandboxConfig =
			serde_json::from_value(serde_json::json!({
				"network": {
					"enabled": true,
					"ports": [
						{"host_port": 9000, "guest_port": 80, "protocol": "tcp", "host_bind": "127.0.0.1"},
						{"host_port": 8081, "guest_port": 81, "protocol": "udp", "host_bind": "0.0.0.0"}
					]
				}
			}))
			.unwrap();
		let ports = from_config(&config);
		assert_eq!(ports.len(), 2);
		assert_eq!(ports[0].to_port_string(), "9000:80");
		assert_eq!(ports[1].to_port_string(), "0.0.0.0:8081:81/udp");
	}

	#[test]
	fn published_ports_empty_when_absent() {
		let config: microsandbox::sandbox::SandboxConfig =
			serde_json::from_value(serde_json::json!({})).unwrap();
		assert!(from_config(&config).is_empty());
	}

	fn write_json(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
		let path = dir.join(name);
		std::fs::write(&path, content).unwrap();
		path
	}

	#[test]
	fn project_ports_absent_returns_none() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(dir.path(), "svamm.json", r#"{"network": {"allow": []}}"#);
		assert_eq!(project_ports(&file).unwrap(), None);
	}

	#[test]
	fn project_ports_parses_canonical_network_ports() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(
			dir.path(),
			"svamm.json",
			r#"{"network": {"allow": [], "ports": ["9000:80", "0.0.0.0:5353:53/udp"]}}"#,
		);
		let mappings = project_ports(&file).unwrap().unwrap();
		assert_eq!(mappings.len(), 2);
		assert_eq!(mappings[0].host_port, 9000);
		assert!(mappings[1].udp);
	}

	#[test]
	fn project_ports_parses_top_level_sugar() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(
			dir.path(),
			"svamm.json",
			r#"{"ports": ["9000:80", "0.0.0.0:5353:53/udp"]}"#,
		);
		let mappings = project_ports(&file).unwrap().unwrap();
		assert_eq!(mappings.len(), 2);
		assert_eq!(mappings[0].host_port, 9000);
		assert!(mappings[1].udp);
	}

	#[test]
	fn project_ports_merges_network_and_top_level_in_order() {
		let dir = tempfile::tempdir().unwrap();
		let file = write_json(
			dir.path(),
			"svamm.json",
			r#"{"network": {"ports": ["9000:80"]}, "ports": ["5353:53/udp"]}"#,
		);
		let mappings = project_ports(&file).unwrap().unwrap();
		assert_eq!(mappings.len(), 2);
		assert_eq!(mappings[0].to_port_string(), "9000:80");
		assert_eq!(mappings[1].to_port_string(), "5353:53/udp");
	}

	#[test]
	fn project_ports_rejects_bad_entries() {
		let dir = tempfile::tempdir().unwrap();
		let bad = write_json(dir.path(), "bad.json", r#"{"ports": ["9000"]}"#);
		assert!(
			project_ports(&bad)
				.unwrap_err()
				.contains("expected HOST:GUEST")
		);

		let bad_network = write_json(
			dir.path(),
			"badnetwork.json",
			r#"{"network": {"ports": ["9000"]}}"#,
		);
		assert!(
			project_ports(&bad_network)
				.unwrap_err()
				.contains("expected HOST:GUEST")
		);

		let not_list = write_json(dir.path(), "notlist.json", r#"{"ports": "9000:80"}"#);
		assert!(
			project_ports(&not_list)
				.unwrap_err()
				.contains("\"ports\" must be a list of strings")
		);

		let network_not_list = write_json(
			dir.path(),
			"networknotlist.json",
			r#"{"network": {"ports": "9000:80"}}"#,
		);
		assert!(
			project_ports(&network_not_list)
				.unwrap_err()
				.contains("\"network.ports\" must be a list of strings")
		);

		let mixed = write_json(dir.path(), "mixed.json", r#"{"ports": ["9000:80", 5]}"#);
		assert!(
			project_ports(&mixed)
				.unwrap_err()
				.contains("\"ports\" must be a list of strings")
		);

		let network_not_object = write_json(
			dir.path(),
			"networknotobject.json",
			r#"{"network": "oops"}"#,
		);
		assert!(
			project_ports(&network_not_object)
				.unwrap_err()
				.contains("expected an object with a \"network\" object")
		);
	}
}
