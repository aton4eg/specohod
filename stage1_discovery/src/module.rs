use std::{fs, path::Path, path::PathBuf};

use anyhow::{Context, Result};
use syn::{File, Item, ItemMod};

use crate::types::ModuleInfo;
use crate::utils::extract_path_attribute;

/// Analyzes a single source file, parsing its AST and recursively resolving
/// inline and file-level submodules.
///
/// # Parameters
///
/// * `file_path` — Path to the `.rs` file.
/// * `crate_name` — Name of the owning crate.
/// * `module_path` — Accumulated module path segments (e.g. `["crate", "foo"]`).
///
/// # Returns
///
/// A `ModuleInfo` describing the module's path and submodule tree.
pub fn analyze_module(
    file_path: &Path,
    crate_name: &str,
    module_path: Vec<String>,
) -> Result<ModuleInfo> {
    let content = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;
    let ast: File = syn::parse_file(&content)
        .with_context(|| format!("Failed to parse file: {}", file_path.display()))?;

    // Directory where `mod name;` file-modules declared in this file live.
    let search_dir = module_search_dir(file_path);
    // Base directory for top-level `#[path = "..."]` attributes: the directory
    // the source file is located in.
    let file_dir = file_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let mut module = ModuleInfo {
        file_path: file_path.to_string_lossy().into_owned(),
        module_path: module_path.join("::"),
        submodules: Vec::new(),
    };

    for item in &ast.items {
        if let Item::Mod(item_mod) = item {
            process_module(
                item_mod,
                &search_dir,
                &file_dir,
                crate_name,
                &module_path,
                &mut module.submodules,
            )?;
        }
    }

    Ok(module)
}

/// Directory in which `mod name;` declarations inside `file_path` resolve their
/// target file, following Rust's rules:
/// - for the crate root (`lib.rs`/`main.rs`) or a `mod.rs`, it is the file's own
///   directory;
/// - otherwise (e.g. `foo.rs`) it is a subdirectory named after the file stem.
fn module_search_dir(file_path: &Path) -> PathBuf {
    let dir = file_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let stem = file_path.file_stem().and_then(|s| s.to_str());
    match stem {
        Some("lib") | Some("main") | Some("mod") => dir,
        Some(other) => dir.join(other),
        None => dir,
    }
}

/// Process a single `mod` item.
///
/// - `search_dir`: directory in which a `mod name;` file-module of the current
///   scope resolves.
/// - `path_attr_base`: base directory for a `#[path = "..."]` attribute at this
///   level.
fn process_module(
    item_mod: &ItemMod,
    search_dir: &Path,
    path_attr_base: &Path,
    crate_name: &str,
    parent_module_path: &[String],
    parent_submodules: &mut Vec<ModuleInfo>,
) -> Result<()> {
    let name = item_mod.ident.to_string();
    let mut current_path: Vec<String> = parent_module_path.to_vec();
    current_path.push(name.clone());

    if let Some((_, items)) = &item_mod.content {
        // Inline module: its file-modules and `#[path]` attributes resolve
        // relative to `search_dir/name`, including the inline module component.
        let inner_dir = search_dir.join(&name);

        let mut inline_module = ModuleInfo {
            file_path: path_attr_base.to_string_lossy().into_owned(),
            module_path: current_path.join("::"),
            submodules: Vec::new(),
        };

        for item in items {
            if let Item::Mod(inner_mod) = item {
                process_module(
                    inner_mod,
                    &inner_dir,
                    &inner_dir,
                    crate_name,
                    &current_path,
                    &mut inline_module.submodules,
                )?;
            }
        }

        parent_submodules.push(inline_module);
    } else {
        let file_path = if let Some(custom) = extract_path_attribute(&item_mod.attrs) {
            path_attr_base.join(custom)
        } else {
            let file1 = search_dir.join(format!("{}.rs", name));
            let file2 = search_dir.join(&name).join("mod.rs");
            if file1.exists() { file1 } else { file2 }
        };

        if file_path.exists() {
            let external_module = analyze_module(&file_path, crate_name, current_path)?;
            parent_submodules.push(external_module);
        }
    }

    Ok(())
}
