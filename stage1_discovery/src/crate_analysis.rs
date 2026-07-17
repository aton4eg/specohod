use crate::types::{CrateInfo, ModuleInfo};
use crate::utils::read_toml;
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::path::Path;

pub fn analyze_crate(root: &Path) -> Result<CrateInfo> {
    let cargo = read_toml(&root.join("Cargo.toml")).context("Failed to read crate Cargo.toml")?;

    let name = cargo["package"]["name"]
        .as_str()
        .context("Missing package.name in Cargo.toml")?
        .to_string();

    let mut entry_points = Vec::with_capacity(2);

    if let Some(lib) = cargo.get("lib") {
        if let Some(path) = lib.get("path").and_then(|v| v.as_str()) {
            entry_points.push(root.join(path));
        }
    } else {
        let lib_path = root.join("src/lib.rs");
        if lib_path.exists() {
            entry_points.push(lib_path);
        }
    }

    if let Some(bins) = cargo.get("bin").and_then(|b| b.as_array()) {
        entry_points.reserve(entry_points.len() + bins.len());
        for bin in bins {
            if let Some(path) = bin.get("path").and_then(|v| v.as_str()) {
                entry_points.push(root.join(path));
            }
        }
    } else {
        let main_path = root.join("src/main.rs");
        if main_path.exists() {
            entry_points.push(main_path);
        }
    }

    let modules: Vec<ModuleInfo> = entry_points
        .par_iter()
        .filter_map(|entry| super::module::analyze_module(entry, &name, vec![name.clone()]).ok())
        .collect();

    Ok(CrateInfo {
        name,
        root_dir: root.to_string_lossy().into_owned(),
        entry_points: entry_points
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        modules,
    })
}
