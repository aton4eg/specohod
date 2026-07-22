use crate::types::ModuleInfo;
use crate::utils::extract_path_attr;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::{fs, hash::Hash, path::Path};
use syn::{File, Item, ItemMod};

pub fn analyze_module(
    file_path: &Path,
    crate_name: &str,
    module_path: Vec<String>,
) -> Result<ModuleInfo> {
    let content = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;
    let _ast: File = syn::parse_file(&content)
        .with_context(|| format!("Failed to parse file: {}", file_path.display()))?;

    let mut visited = HashMap::new();
    analyze_module_impl(file_path, crate_name, module_path, &mut visited)
}

fn analyze_module_impl(
    file_path: &Path,
    crate_name: &str,
    module_path: Vec<String>,
    visited: &mut HashMap<Vec<String>, ModuleInfo>,
) -> Result<ModuleInfo>
where
    Vec<String>: Hash + Eq,
{
    let key = module_path.clone();
    if let Some(cached) = visited.get(&key) {
        return Ok(cached.clone());
    }

    let content = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;
    let ast: File = syn::parse_file(&content)
        .with_context(|| format!("Failed to parse file: {}", file_path.display()))?;

    let mut queue = Vec::new();

    for item in &ast.items {
        if let Item::Mod(m) = item {
            queue.push((m, file_path.to_path_buf()));
        }
    }

    let mut processed_module = ModuleInfo {
        file_path: file_path.to_string_lossy().into_owned(),
        module_path: module_path.join("::"),
        submodules: Vec::new(),
    };

    for (item_mod, parent_file) in queue {
        process_module_queue(
            item_mod,
            &parent_file,
            crate_name,
            &module_path,
            &mut processed_module.submodules,
            visited,
        )?;
    }

    let result = if visited.contains_key(&module_path) {
        visited.get(&module_path).unwrap().clone()
    } else {
        processed_module
    };

    visited.insert(module_path, result.clone());
    Ok(result)
}

fn process_module_queue(
    item_mod: &ItemMod,
    parent_file: &Path,
    crate_name: &str,
    parent_module_path: &[String],
    parent_submodules: &mut Vec<ModuleInfo>,
    visited: &mut HashMap<Vec<String>, ModuleInfo>,
) -> Result<()> {
    let name = item_mod.ident.to_string();
    let mut current_path: Vec<String> = parent_module_path.to_vec();
    current_path.push(name.clone());

    if let Some((_, items)) = &item_mod.content {
        let mut queue = Vec::new();

        for item in items {
            if let Item::Mod(inner_mod) = item {
                queue.push((inner_mod, parent_file.to_path_buf()));
            }
        }

        let mut inline_module = ModuleInfo {
            file_path: parent_file.to_string_lossy().into_owned(),
            module_path: current_path.join("::"),
            submodules: Vec::new(),
        };

        for (child_mod, child_parent_file) in queue {
            process_module_queue(
                child_mod,
                &child_parent_file,
                crate_name,
                &current_path,
                &mut inline_module.submodules,
                visited,
            )?;
        }

        let final_module = if visited.contains_key(&current_path) {
            visited.get(&current_path).unwrap().clone()
        } else {
            inline_module
        };

        visited.insert(current_path.clone(), final_module.clone());
        parent_submodules.push(final_module);
    } else {
        let base_dir = parent_file
            .parent()
            .context("Module file has no parent directory")?;

        let path_attr = extract_path_attr(&item_mod.attrs);

        let file_path = if let Some(custom) = path_attr {
            base_dir.join(custom)
        } else {
            let file1 = base_dir.join(format!("{}.rs", name));
            let file2 = base_dir.join(&name).join("mod.rs");

            if file1.exists() { file1 } else { file2 }
        };

        if file_path.exists() {
            let external_module =
                analyze_module_impl(&file_path, crate_name, current_path, visited)?;
            parent_submodules.push(external_module);
        }
    }

    Ok(())
}
