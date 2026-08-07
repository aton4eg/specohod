use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use log::info;
use petgraph::Direction;
use petgraph::graph::NodeIndex;

use crate::collector;
use crate::hash::{compute_hash, extract_hash_from_spec};

fn delete_partial_specs(module: &collector::Module) -> Result<()> {
    let dir = module
        .spec_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));

    let stem_spec = module
        .spec_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let stem_prelim = module
        .prelim_spec_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();

    for entry in std::fs::read_dir(dir).with_context(|| {
        format!(
            "Failed to read directory {:?} when searching for partial specs",
            dir
        )
    })? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let fname = path.file_name().unwrap_or_default().to_string_lossy();

        if fname.ends_with(".spec.md") {
            for stem in &[&*stem_spec, &*stem_prelim] {
                if fname.starts_with(*stem) {
                    let rest = &fname[stem.len()..];
                    if rest.starts_with(".partial.") {
                        std::fs::remove_file(&path)
                            .with_context(|| format!("Failed to remove partial spec {:?}", path))?;
                        log::info!("Removed partial spec: {:?}", path);
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn incremental_update(
    modules: &[collector::Module],
    graph: &petgraph::graph::DiGraph<PathBuf, ()>,
    node_map: &HashMap<PathBuf, NodeIndex>,
) -> Result<()> {
    let mut changed = HashSet::with_capacity(modules.len());

    for module in modules {
        let code = std::fs::read_to_string(&module.file_path)
            .with_context(|| format!("Failed to read {:?}", module.file_path))?;
        let current_hash = compute_hash(&code);

        let need_update = if module.spec_path.exists() {
            match extract_hash_from_spec(&module.spec_path) {
                Some(saved_hash) => saved_hash != current_hash,
                None => true,
            }
        } else {
            true
        };

        if need_update {
            changed.insert(module.file_path.clone());
        }
    }

    if changed.is_empty() {
        info!("No changes detected, skipping incremental update.");
        return Ok(());
    }
    info!("Detected {} changed modules.", changed.len());

    let mut affected: HashSet<PathBuf> = changed.clone();
    let mut queue: Vec<PathBuf> = changed.iter().cloned().collect();

    while let Some(current) = queue.pop() {
        if let Some(&node) = node_map.get(&current) {
            for dep_node in graph.neighbors_directed(node, Direction::Incoming) {
                let dep_path = graph.node_weight(dep_node).unwrap().clone();
                if affected.insert(dep_path.clone()) {
                    queue.push(dep_path);
                }
            }
        }
    }

    info!(
        "Affected modules (including dependants): {}",
        affected.len()
    );

    for module in modules {
        if affected.contains(&module.file_path) {
            if module.spec_path.exists() {
                std::fs::remove_file(&module.spec_path)
                    .with_context(|| format!("Failed to remove {:?}", module.spec_path))?;
                info!("Removed final spec: {:?}", module.spec_path);
            }
            if module.prelim_spec_path.exists() {
                std::fs::remove_file(&module.prelim_spec_path)
                    .with_context(|| format!("Failed to remove {:?}", module.prelim_spec_path))?;
                info!("Removed preliminary spec: {:?}", module.prelim_spec_path);
            }
            delete_partial_specs(module)?;
        }
    }

    Ok(())
}
