mod crate_analysis;
mod module;
mod types;
mod utils;
mod workspace;

use std::path::Path;

use anyhow::{Context, Result};

fn main() -> Result<()> {
    let root = std::env::args()
        .nth(1)
        .context("Usage: stage1_discovery <workspace_path>")?;

    let root_path = Path::new(&root);
    if !root_path.exists() {
        anyhow::bail!("Path does not exist: {}", root);
    }

    if !root_path.is_dir() {
        anyhow::bail!("Path is not a directory: {}", root);
    }

    let cargo_toml = root_path.join("Cargo.toml");
    if !cargo_toml.exists() {
        anyhow::bail!("Cargo.toml not found at: {}", cargo_toml.display());
    }

    let graph = workspace::discover_workspace(root_path)?;
    println!("{}", serde_json::to_string_pretty(&graph)?);
    Ok(())
}
