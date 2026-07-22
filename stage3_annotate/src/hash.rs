use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

pub fn compute_hash(contents: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(contents.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn extract_hash_from_spec(spec_path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(spec_path).ok()?;
    let first_line = content.lines().next()?;
    let trimmed = first_line.trim();
    trimmed
        .strip_prefix("<!-- SPEC_HASH: ")
        .and_then(|s| s.strip_suffix(" -->"))
        .map(|inner| inner.to_string())
}

pub fn write_spec_with_hash(path: &std::path::Path, spec_body: &str, hash: &str) -> Result<()> {
    let full = format!("<!-- SPEC_HASH: {} -->\n{}", hash, spec_body);
    std::fs::write(path, full).with_context(|| format!("Failed to write spec to {:?}", path))?;
    Ok(())
}
