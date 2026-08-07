use std::{fs, path::Path};

use anyhow::{Context, Result};

/// Reads and parses a TOML file at the given path.
///
/// # Parameters
///
/// * `path` — Path to the TOML file.
///
/// # Returns
///
/// The parsed `toml::Value`.
pub fn read_toml(path: &Path) -> Result<toml::Value> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read TOML file: {}", path.display()))?;
    toml::from_str(&content).context("Failed to parse TOML content")
}

/// Extracts the value of a `#[path = "..."]` attribute from a list of attributes.
///
/// # Parameters
///
/// * `attrs` — Slice of `syn::Attribute` to search.
///
/// # Returns
///
/// The path string if a `#[path = "..."]` attribute was found, otherwise `None`.
pub fn extract_path_attribute(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if attr.path().is_ident("path")
            && let syn::Meta::NameValue(meta) = &attr.meta
            && let syn::Expr::Lit(expr_lit) = &meta.value
            && let syn::Lit::Str(lit_str) = &expr_lit.lit
        {
            return Some(lit_str.value());
        }
        None
    })
}
