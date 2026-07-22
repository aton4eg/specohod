use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::types::{CrateInfo, ModuleInfo};
use crate::utils::read_toml;

fn collect_entry_points(root: &Path, cargo: &toml::Value) -> Vec<PathBuf> {
    let mut entry_points = Vec::new();

    // Library target: an explicit `[lib] path`, otherwise the conventional
    // `src/lib.rs` (which also applies when `[lib]` is present without a path).
    if let Some(path) = cargo
        .get("lib")
        .and_then(|l| l.get("path"))
        .and_then(|v| v.as_str())
    {
        entry_points.push(root.join(path));
    } else {
        let lib_path = root.join("src/lib.rs");
        if lib_path.exists() {
            entry_points.push(lib_path);
        }
    }

    // Explicit `[[bin]]` targets with a declared path.
    if let Some(bins) = cargo.get("bin").and_then(|b| b.as_array()) {
        for bin in bins {
            if let Some(path) = bin.get("path").and_then(|v| v.as_str()) {
                let p = root.join(path);
                if !entry_points.contains(&p) {
                    entry_points.push(p);
                }
            }
        }
    }

    // Default binary `src/main.rs` (cargo auto-discovers it regardless of
    // explicit `[[bin]]` entries).
    let main_path = root.join("src/main.rs");
    if main_path.exists() && !entry_points.contains(&main_path) {
        entry_points.push(main_path);
    }

    // Auto-discovered binaries under `src/bin/`.
    let bin_dir = root.join("src/bin");
    if bin_dir.is_dir()
        && let Ok(entries) = std::fs::read_dir(&bin_dir)
    {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("rs") && !entry_points.contains(&p) {
                entry_points.push(p);
            }
        }
    }

    entry_points
}

pub fn analyze_crate(root: &Path) -> Result<CrateInfo> {
    let cargo = read_toml(&root.join("Cargo.toml")).context("Failed to read crate Cargo.toml")?;

    let name = cargo["package"]["name"]
        .as_str()
        .context("Missing package.name in Cargo.toml")?
        .to_string();

    let entry_points = collect_entry_points(root, &cargo);

    let modules: Vec<ModuleInfo> = entry_points
        .iter()
        .filter_map(
            |entry| match super::module::analyze_module(entry, &name, vec![name.clone()]) {
                Ok(module) => Some(module),
                Err(e) => {
                    eprintln!(
                        "Warning: failed to analyze entry point {}: {:#}",
                        entry.display(),
                        e
                    );
                    None
                }
            },
        )
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
