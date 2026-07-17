use anyhow::{Context, Result};
use std::{fs, path::Path};

pub fn read_toml(path: &Path) -> Result<toml::Value> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read TOML file: {}", path.display()))?;
    toml::from_str(&content).context("Failed to parse TOML content")
}

pub fn extract_path_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if attr.path().is_ident("path") {
            if let syn::Meta::NameValue(meta) = &attr.meta {
                if let syn::Expr::Lit(expr_lit) = &meta.value {
                    if let syn::Lit::Str(lit_str) = &expr_lit.lit {
                        return Some(lit_str.value());
                    }
                }
            }
        }
        None
    })
}
