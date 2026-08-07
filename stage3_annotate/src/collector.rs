use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use walkdir::WalkDir;

#[derive(Debug, Deserialize)]
struct DepsFile {
    dependency: Vec<Dependency>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct Dependency {
    module_path: String,
    file_path: String,
    spec_path: String,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub file_path: PathBuf,
    pub deps_path: PathBuf,
    pub facts_path: PathBuf,
    pub spec_path: PathBuf,
    pub prelim_spec_path: PathBuf,
}

/// Walks the project directory and collects all `.rs` files that have
/// corresponding `.deps.json` and `.facts.json` sidecar files.
///
/// Skips hidden directories and test/example directories.
///
/// # Parameters
///
/// * `root` — The project root directory.
///
/// # Returns
///
/// A list of `Module` records describing each module and its associated files.
pub fn collect_modules(root: &Path) -> Result<Vec<Module>> {
    let mut modules = Vec::new();
    let root = root.canonicalize().context("Failed to canonicalize root")?;

    for entry in WalkDir::new(&root)
        .into_iter()
        .filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'))
    {
        let entry = entry?;
        let path = entry.path();

        if path.is_file()
            && path.extension().and_then(|s| s.to_str()) == Some("rs")
            && !path.components().any(|c| c.as_os_str() == "tests")
            && !path.components().any(|c| c.as_os_str() == "examples")
        {
            let rs_path = path.to_path_buf();
            let deps_path = rs_path.with_extension("deps.json");
            let facts_path = rs_path.with_extension("facts.json");
            let spec_path = rs_path.with_extension("spec.md");
            let prelim_spec_path = rs_path.with_extension("preliminary.spec.md");

            if deps_path.exists() && facts_path.exists() {
                modules.push(Module {
                    file_path: rs_path,
                    deps_path,
                    facts_path,
                    spec_path,
                    prelim_spec_path,
                });
            } else {
                log::debug!("Skipping {:?}: missing .deps.json or .facts.json", rs_path);
            }
        }
    }

    Ok(modules)
}

/// Reads a `.deps.json` file and resolves all dependency paths, attempting
/// both absolute and relative-to-deps-file resolution.
///
/// # Parameters
///
/// * `deps_path` — Path to the `.deps.json` file.
///
/// # Returns
///
/// A list of canonicalized `PathBuf`s for each dependency.
pub fn read_dependencies(deps_path: &Path) -> Result<Vec<PathBuf>> {
    let content = std::fs::read_to_string(deps_path)
        .with_context(|| format!("Failed to read {:?}", deps_path))?;
    let deps: DepsFile = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {:?}", deps_path))?;

    let mut paths = Vec::with_capacity(deps.dependency.len());
    for dep in deps.dependency {
        let path = PathBuf::from(&dep.file_path);
        match path.canonicalize() {
            Ok(canon) => paths.push(canon),
            Err(_) => {
                // Try resolving relative to the deps.json file's directory
                if let Some(parent) = deps_path.parent()
                    && let Ok(canon) = parent.join(&path).canonicalize()
                {
                    paths.push(canon);
                    continue;
                }
                log::warn!(
                    "Could not canonicalize dependency path {} (checked absolute and relative to {:?})",
                    dep.file_path,
                    deps_path.parent(),
                );
                paths.push(path);
            }
        }
    }
    Ok(paths)
}
