//! user-facing output helpers (colors, tables, step/die/note messages).

use console::style;

/// minimal table renderer with column alignment.
///
/// header rows render in cyan bold. cell alignment uses visible width so ANSI
/// escapes in cell values do not break the columns.
pub struct Table {
	headers: Vec<String>,
	rows: Vec<Vec<String>>,
}

impl Table {
	/// create a table with the given column headers.
	pub fn new(headers: &[&str]) -> Self {
		Self {
			headers: headers.iter().map(|header| header.to_string()).collect(),
			rows: Vec::new(),
		}
	}

	/// add a row to the table.
	pub fn add_row(&mut self, row: Vec<String>) {
		self.rows.push(row);
	}

	/// print the table to stdout. do nothing when there are no rows.
	pub fn print(&self) {
		let rendered = self.render();
		if !rendered.is_empty() {
			print!("{rendered}");
		}
	}

	/// render the table to a string with column alignment.
	/// uses visible width so ANSI escapes in cells do not break the columns.
	pub fn render(&self) -> String {
		if self.rows.is_empty() {
			return String::new();
		}

		let col_count = self.headers.len();
		let mut widths: Vec<usize> = self.headers.iter().map(|header| header.len()).collect();
		for row in &self.rows {
			for (index, cell) in row.iter().enumerate() {
				if index < col_count {
					widths[index] = widths[index].max(console::measure_text_width(cell));
				}
			}
		}

		let mut out = String::new();
		let header: String = self
			.headers
			.iter()
			.enumerate()
			.map(|(index, header)| {
				if index < col_count - 1 {
					format!("{:<width$}    ", header, width = widths[index])
				} else {
					header.clone()
				}
			})
			.collect();
		out.push_str(&style(header).cyan().bold().to_string());
		out.push('\n');

		for row in &self.rows {
			let line: String = row
				.iter()
				.enumerate()
				.map(|(index, cell)| {
					if index < col_count - 1 {
						let visible = console::measure_text_width(cell);
						let padding = widths[index].saturating_sub(visible) + 4;
						format!("{cell}{:padding$}", "", padding = padding)
					} else {
						cell.clone()
					}
				})
				.collect();
			out.push_str(&line);
			out.push('\n');
		}
		out
	}
}

/// format a sandbox status with the msb status colors.
pub fn format_status(status: &str) -> String {
	match status {
		"created" | "Created" => format!("{}", style("created").dim()),
		"starting" | "Starting" => format!("{}", style("starting").yellow().bold()),
		"running" | "Running" => format!("{}", style("running").green().bold()),
		"stopped" | "Stopped" => format!("{}", style("stopped").dim()),
		"paused" | "Paused" => format!("{}", style("paused").yellow().bold()),
		"draining" | "Draining" => format!("{}", style("draining").yellow().bold()),
		"crashed" | "Crashed" => format!("{}", style("crashed").red().bold()),
		other => other.to_lowercase(),
	}
}

/// print a progress step.
pub fn step(message: impl AsRef<str>) {
	println!("{} {}", style("[+]").green(), message.as_ref());
}

/// print a success message.
pub fn ok(message: impl AsRef<str>) {
	println!("{} {}", style("[+]").green().bold(), message.as_ref());
}

/// print a note.
pub fn note(message: impl AsRef<str>) {
	println!("{} {}", style("    note:").green().bold(), message.as_ref());
}

/// print a warning note.
pub fn warn_note(message: impl AsRef<str>) {
	println!("{} {}", style("[*]").yellow().bold(), message.as_ref());
}

/// print a pruned-item message.
pub fn pruned(message: impl AsRef<str>) {
	println!(
		"{} {}",
		style("    pruned:").green().bold(),
		message.as_ref()
	);
}

/// print a warning.
pub fn warn(message: impl AsRef<str>) {
	println!("{} {}", style("[*]").yellow().bold(), message.as_ref());
}

/// the styled fatal-error marker.
pub fn style_error() -> String {
	format!("{}", style("[!]").red().bold())
}

/// report a single image-pull progress event to the user.
///
/// only meaningful milestone events are printed. byte-level progress events
/// are too frequent and skipped, so output stays readable.
pub fn pull_progress(reference: &str, event: &microsandbox_image::PullProgress) {
	if let Some(line) = pull_progress_line(reference, event) {
		println!("    {}", style(line).dim());
	}
}

/// render a pull-progress event as a line, or None to print nothing.
fn pull_progress_line(reference: &str, event: &microsandbox_image::PullProgress) -> Option<String> {
	match event {
		microsandbox_image::PullProgress::Resolving { .. } => {
			Some(format!("pulling {reference}..."))
		}
		microsandbox_image::PullProgress::Resolved {
			layer_count,
			total_download_bytes,
			..
		} => match total_download_bytes {
			Some(bytes) => Some(format!(
				"resolved {reference} ({} layer{}, {})",
				layer_count,
				if *layer_count == 1 { "" } else { "s" },
				human_bytes(*bytes)
			)),
			None => Some(format!(
				"resolved {reference} ({} layer{})",
				layer_count,
				if *layer_count == 1 { "" } else { "s" },
			)),
		},
		microsandbox_image::PullProgress::LayerDownloadComplete {
			layer_index,
			downloaded_bytes,
			..
		} => Some(format!(
			"layer {} downloaded ({})",
			layer_index + 1,
			human_bytes(*downloaded_bytes)
		)),
		microsandbox_image::PullProgress::LayerMaterializeStarted { layer_index, .. } => {
			Some(format!("layer {} materializing", layer_index + 1))
		}
		microsandbox_image::PullProgress::LayerMaterializeComplete { layer_index, .. } => {
			Some(format!("layer {} materialized", layer_index + 1))
		}
		microsandbox_image::PullProgress::StitchMergingTrees { layer_count } => {
			Some(format!("merging {layer_count} layers"))
		}
		microsandbox_image::PullProgress::StitchWritingFsmeta => {
			Some("writing filesystem metadata".to_string())
		}
		microsandbox_image::PullProgress::StitchWritingVmdk => {
			Some("writing disk image".to_string())
		}
		microsandbox_image::PullProgress::StitchComplete => Some("image materialized".to_string()),
		microsandbox_image::PullProgress::Complete { .. } => Some("pull complete".to_string()),
		microsandbox_image::PullProgress::LayerDownloadProgress { .. }
		| microsandbox_image::PullProgress::LayerDownloadVerifying { .. }
		| microsandbox_image::PullProgress::LayerMaterializeProgress { .. }
		| microsandbox_image::PullProgress::LayerMaterializeWriting { .. } => None,
	}
}

/// format a byte count in human readable units.
fn human_bytes(bytes: u64) -> String {
	const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
	let mut value = bytes as f64;
	let mut unit = 0;
	while value >= 1024.0 && unit < UNITS.len() - 1 {
		value /= 1024.0;
		unit += 1;
	}
	if unit == 0 {
		format!("{bytes} B")
	} else {
		format!("{value:.1} {}", UNITS[unit])
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ok_message_contains_text() {
		let output = format!("{}", style("ok:").green().bold());
		assert!(output.contains("ok:"));
	}

	#[test]
	fn pull_progress_renders_milestones() {
		let reference = "ghcr.io/acme/vm:9";
		let line = pull_progress_line(
			reference,
			&microsandbox_image::PullProgress::Resolved {
				reference: reference.into(),
				manifest_digest: "sha256:abc".into(),
				layer_count: 2,
				total_download_bytes: Some(1_048_576),
			},
		)
		.unwrap();
		assert_eq!(line, "resolved ghcr.io/acme/vm:9 (2 layers, 1.0 MiB)");

		let line = pull_progress_line(
			reference,
			&microsandbox_image::PullProgress::LayerDownloadComplete {
				layer_index: 0,
				digest: "sha256:abc".into(),
				downloaded_bytes: 512,
			},
		)
		.unwrap();
		assert_eq!(line, "layer 1 downloaded (512 B)");

		let line = pull_progress_line(
			reference,
			&microsandbox_image::PullProgress::LayerMaterializeComplete {
				layer_index: 1,
				diff_id: "sha256:def".into(),
			},
		)
		.unwrap();
		assert_eq!(line, "layer 2 materialized");
	}

	#[test]
	fn pull_progress_skips_byte_level_events() {
		let reference = "ghcr.io/acme/vm:9";
		assert!(
			pull_progress_line(
				reference,
				&microsandbox_image::PullProgress::LayerDownloadProgress {
					layer_index: 0,
					digest: "sha256:abc".into(),
					downloaded_bytes: 1,
					total_bytes: Some(100),
				}
			)
			.is_none()
		);
		assert!(
			pull_progress_line(
				reference,
				&microsandbox_image::PullProgress::LayerMaterializeProgress {
					layer_index: 0,
					bytes_read: 1,
					total_bytes: 100,
				}
			)
			.is_none()
		);
	}

	#[test]
	fn human_bytes_formats() {
		assert_eq!(human_bytes(0), "0 B");
		assert_eq!(human_bytes(512), "512 B");
		assert_eq!(human_bytes(1536), "1.5 KiB");
		assert_eq!(human_bytes(1_048_576), "1.0 MiB");
	}
}
