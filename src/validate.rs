//! input validation helpers (sandbox slugs, disk sizes).

/// true when `name` is a valid sandbox slug: starts with an alphanumeric
/// character, contains only `[A-Za-z0-9._-]`, and is at most 128 bytes.
pub fn is_slug(name: &str) -> bool {
	let mut bytes = name.bytes();
	match bytes.next() {
		Some(first) if first.is_ascii_alphanumeric() => {}
		_ => return false,
	}
	if name.len() > 128 {
		return false;
	}
	bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// validate a sandbox name.
pub fn require_slug(name: &str) -> Result<(), String> {
	if !is_slug(name) {
		return Err(format!(
            "invalid sandbox name '{name}': must be a slug (alphanumeric, '.', '_', '-'; no spaces; max 128 chars)"
        ));
	}
	Ok(())
}

/// true when `size` is a flat root disk size: digits plus an optional single
/// unit suffix (`K/M/G/T/P`).
pub fn is_disk_size(size: &str) -> bool {
	let bytes = size.as_bytes();
	if bytes.is_empty() {
		return false;
	}
	let mut index = 0;
	while index < bytes.len() && bytes[index].is_ascii_digit() {
		index += 1;
	}
	if index == 0 {
		return false;
	}
	if index == bytes.len() {
		return true;
	}
	index == bytes.len() - 1
		&& matches!(
			bytes[index].to_ascii_uppercase(),
			b'K' | b'M' | b'G' | b'T' | b'P'
		)
}

/// validate a root disk size.
pub fn require_disk_size(size: &str) -> Result<(), String> {
	if !is_disk_size(size) {
		return Err(format!(
			"invalid root disk size '{size}' (expected e.g. 16G or 2048M)"
		));
	}
	Ok(())
}

/// validate a guest tmpfs (/tmp) size.
pub fn require_tmpfs_size(size: &str) -> Result<(), String> {
	if !is_disk_size(size) {
		return Err(format!(
			"invalid tmpfs size '{size}' (expected e.g. 2G or 2048M)"
		));
	}
	Ok(())
}

/// validate a memory allocation size.
pub fn require_memory_size(size: &str) -> Result<(), String> {
	if !is_disk_size(size) {
		return Err(format!(
			"invalid memory size '{size}' (expected e.g. 8G or 4096M)"
		));
	}
	Ok(())
}

/// true when `quota` is a workspace mount quota: digits plus an optional
/// single unit suffix (`M` or `G`), or a plain count.
pub fn is_quota_size(quota: &str) -> bool {
	let bytes = quota.as_bytes();
	if bytes.is_empty() {
		return false;
	}
	let mut index = 0;
	while index < bytes.len() && bytes[index].is_ascii_digit() {
		index += 1;
	}
	if index == 0 {
		return false;
	}
	if index == bytes.len() {
		return true;
	}
	index == bytes.len() - 1 && matches!(bytes[index].to_ascii_uppercase(), b'M' | b'G')
}

/// validate a workspace mount quota.
pub fn require_quota_size(quota: &str) -> Result<(), String> {
	if !is_quota_size(quota) {
		return Err(format!(
			"invalid workspace mount quota '{quota}' (expected e.g. 32G or 4096M)"
		));
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn slug_accepts_valid_names() {
		for name in ["alpha", "foo-bar_1.b", "a", &"a".repeat(128)] {
			assert!(is_slug(name), "expected {name:?} to be accepted");
		}
	}

	#[test]
	fn slug_rejects_invalid_names() {
		for name in [
			"",
			"bad name",
			"-lead",
			&"a".repeat(129),
			"a/b",
			"a:b",
			"✨",
		] {
			assert!(!is_slug(name), "expected {name:?} to be rejected");
		}
	}

	#[test]
	fn disk_size_accepts_valid_sizes() {
		for size in ["16G", "2048M", "1T", "8g", "1024", "512P", "64K"] {
			assert!(is_disk_size(size), "expected {size:?} to be accepted");
		}
	}

	#[test]
	fn disk_size_rejects_invalid_sizes() {
		for size in ["", "banana", "16GB", "G16", "16.5G", "-16G", " 16G", "16Gx"] {
			assert!(!is_disk_size(size), "expected {size:?} to be rejected");
		}
	}

	#[test]
	fn quota_size_accepts_valid_sizes() {
		for size in ["32G", "4096M", "1024", "8g", "512m", "1G"] {
			assert!(is_quota_size(size), "expected {size:?} to be accepted");
		}
	}

	#[test]
	fn quota_size_rejects_invalid_sizes() {
		for size in [
			"", "banana", "32GB", "G32", "32.5G", "-32G", " 32G", "32K", "32T", "0x10",
		] {
			assert!(!is_quota_size(size), "expected {size:?} to be rejected");
		}
	}
}
