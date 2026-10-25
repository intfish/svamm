//! network policy rendering.

use console::style;
use microsandbox_types::{
	Action, Destination, DestinationGroup, Direction, NetworkPolicy, PortRange, Protocol,
};

use crate::ui;

/// render a network policy to a human-readable listing.
pub fn render(policy: &NetworkPolicy) -> String {
	let mut output = String::new();
	output.push_str(&format!(
		"default egress:  {}\n",
		action(&policy.default_egress)
	));
	output.push_str(&format!(
		"default ingress: {}\n",
		action(&policy.default_ingress)
	));
	output.push_str(&format!(
		"rules ({}):\n",
		style(policy.rules.len().to_string()).cyan().bold()
	));
	let mut table = ui::Table::new(&["ACTION", "DIRECTION", "DEST", "PROTOCOL", "PORT"]);
	for rule in &policy.rules {
		table.add_row(vec![
			action(&rule.action),
			direction(&rule.direction).to_string(),
			dest(&rule.destination),
			protos(&rule.protocols),
			ports(&rule.ports),
		]);
	}
	output.push_str(&table.render());
	output
}

fn action(action: &Action) -> String {
	match action {
		Action::Allow => format!("{}", style("allow").green().bold()),
		Action::Deny => format!("{}", style("deny").red().bold()),
	}
}

fn direction(direction: &Direction) -> &'static str {
	match direction {
		Direction::Egress => "egress",
		Direction::Ingress => "ingress",
		Direction::Any => "any",
	}
}

fn dest(destination: &Destination) -> String {
	match destination {
		Destination::Any => "any".to_string(),
		Destination::Cidr(network) => network.to_string(),
		Destination::Domain(name) => name.clone(),
		Destination::DomainSuffix(suffix) => {
			if suffix.starts_with("*.") {
				suffix.clone()
			} else {
				format!("*.{suffix}")
			}
		}
		Destination::Group(group) => group_name(group).to_string(),
	}
}

fn group_name(group: &DestinationGroup) -> &'static str {
	match group {
		DestinationGroup::Public => "public",
		DestinationGroup::Loopback => "loopback",
		DestinationGroup::Private => "private",
		DestinationGroup::LinkLocal => "link-local",
		DestinationGroup::Metadata => "meta",
		DestinationGroup::Multicast => "multicast",
		DestinationGroup::Host => "host",
	}
}

fn protos(protocols: &[Protocol]) -> String {
	if protocols.is_empty() {
		return "any".to_string();
	}
	protocols
		.iter()
		.map(|protocol| match protocol {
			Protocol::Tcp => "tcp",
			Protocol::Udp => "udp",
			Protocol::Icmpv4 => "icmpv4",
			Protocol::Icmpv6 => "icmpv6",
		})
		.collect::<Vec<_>>()
		.join(",")
}

fn ports(ports: &[PortRange]) -> String {
	if ports.is_empty() {
		return "any".to_string();
	}
	let rendered: Vec<String> = ports
		.iter()
		.map(|range| {
			if range.end == range.start {
				range.start.to_string()
			} else {
				format!("{}-{}", range.start, range.end)
			}
		})
		.collect();
	if rendered.is_empty() {
		"any".to_string()
	} else {
		rendered.join(",")
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use microsandbox_types::{Destination, Direction, PortRange, Protocol, Rule};

	fn rule(
		direction: Direction,
		destination: Destination,
		protocols: Vec<Protocol>,
		ports: Vec<PortRange>,
		action: Action,
	) -> Rule {
		Rule {
			direction,
			destination,
			protocols,
			ports,
			action,
		}
	}

	#[test]
	fn render_matches_format() {
		let policy = NetworkPolicy {
			default_egress: Action::Allow,
			default_ingress: Action::Deny,
			rules: vec![
				rule(
					Direction::Egress,
					Destination::Group(DestinationGroup::Host),
					vec![Protocol::Tcp],
					vec![PortRange {
						start: 50000,
						end: 50000,
					}],
					Action::Allow,
				),
				rule(
					Direction::Egress,
					Destination::Domain("example.com".to_string()),
					vec![Protocol::Tcp],
					vec![PortRange {
						start: 443,
						end: 443,
					}],
					Action::Allow,
				),
				rule(
					Direction::Egress,
					Destination::DomainSuffix("tracking.com".to_string()),
					vec![],
					vec![],
					Action::Deny,
				),
			],
		};
		let rendered = render(&policy);
		let lines: Vec<String> = rendered
			.lines()
			.map(|line| console::strip_ansi_codes(line).into_owned())
			.collect();
		assert_eq!(lines[0], "default egress:  allow");
		assert_eq!(lines[1], "default ingress: deny");
		assert_eq!(lines[2], "rules (3):");
		// table format: aligned columns under matching headers.
		assert_eq!(
			lines[3].split_whitespace().collect::<Vec<_>>(),
			vec!["ACTION", "DIRECTION", "DEST", "PROTOCOL", "PORT"]
		);
		assert_eq!(
			lines[4].split_whitespace().collect::<Vec<_>>(),
			vec!["allow", "egress", "host", "tcp", "50000"]
		);
		assert_eq!(
			lines[5].split_whitespace().collect::<Vec<_>>(),
			vec!["allow", "egress", "example.com", "tcp", "443"]
		);
		assert_eq!(
			lines[6].split_whitespace().collect::<Vec<_>>(),
			vec!["deny", "egress", "*.tracking.com", "any", "any"]
		);
	}

	#[test]
	fn render_dest_forms() {
		assert_eq!(dest(&Destination::Any), "any");
		assert_eq!(
			dest(&Destination::Cidr("10.0.0.0/8".parse().unwrap())),
			"10.0.0.0/8"
		);
		assert_eq!(dest(&Destination::Domain("x.com".to_string())), "x.com");
		assert_eq!(
			dest(&Destination::DomainSuffix("tracking.com".to_string())),
			"*.tracking.com"
		);
		assert_eq!(
			dest(&Destination::DomainSuffix("*.tracking.com".to_string())),
			"*.tracking.com"
		);
		assert_eq!(dest(&Destination::Group(DestinationGroup::Host)), "host");
		assert_eq!(
			dest(&Destination::Group(DestinationGroup::Public)),
			"public"
		);
	}
}
