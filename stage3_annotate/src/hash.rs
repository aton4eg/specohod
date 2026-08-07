use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// Computes a SHA-256 hex hash of a string.
///
/// # Parameters
///
/// * `contents` — The input string.
///
/// # Returns
///
/// The hex-encoded hash string.
pub fn compute_hash(contents: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(contents.as_bytes());
    hex::encode(hasher.finalize())
}

/// Reads a spec file and extracts the `SPEC_HASH` embedded in the first line.
///
/// Expected format: `<!-- SPEC_HASH: <hex> -->`
///
/// # Parameters
///
/// * `spec_path` — Path to a `.spec.md` file.
///
/// # Returns
///
/// The hash string if present and well-formed, otherwise `None`.
pub fn extract_hash_from_spec(spec_path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(spec_path).ok()?;
    let first_line = content.lines().next()?;
    let trimmed = first_line.trim();
    trimmed
        .strip_prefix("<!-- SPEC_HASH: ")
        .and_then(|s| s.strip_suffix(" -->"))
        .map(|inner| inner.to_string())
}

/// Writes a spec file with a `SPEC_HASH` comment prepended on the first line.
///
/// # Parameters
///
/// * `path` — Output path for the spec file.
/// * `spec_body` — The spec content.
/// * `hash` — The hash to embed.
pub fn write_spec_with_hash(path: &std::path::Path, spec_body: &str, hash: &str) -> Result<()> {
    let full = format!("<!-- SPEC_HASH: {} -->\n{}", hash, spec_body);
    std::fs::write(path, full).with_context(|| format!("Failed to write spec to {:?}", path))?;
    Ok(())
}
