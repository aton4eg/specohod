use std::path::Path;

use anyhow::{Context, Result};
use glob::glob;
use rayon::prelude::*;

use crate::types::WorkspaceGraph;
use crate::utils::read_toml;

pub fn discover_workspace(root: &Path) -> Result<WorkspaceGraph> {
    let cargo =
        read_toml(&root.join("Cargo.toml")).context("Failed to read workspace Cargo.toml")?;

    let mut crate_paths = Vec::new();

    match cargo.get("workspace") {
        Some(workspace) => {
            if let Some(members) = workspace.get("members").and_then(|m| m.as_array()) {
                for member in members {
                    let member_str = member
                        .as_str()
                        .context("Workspace member is not a string")?;
                    let pattern = root.join(member_str);
                    let pattern = pattern.to_string_lossy().into_owned();

                    for entry in glob(&pattern)
                        .with_context(|| format!("Failed to glob pattern: {}", pattern))?
                    {
                        let path = entry.context("Failed to iter glob entry")?;
                        if path.join("Cargo.toml").exists() {
                            crate_paths.push(path);
                        }
                    }
                }
            }
        }
        None => crate_paths.push(root.to_path_buf()),
    }

    let crates: Vec<_> = crate_paths
        .par_iter()
        .map(|path| {
            super::crate_analysis::analyze_crate(path)
                .with_context(|| format!("Failed to analyze crate at: {}", path.display()))
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(WorkspaceGraph { crates })
}
