use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use petgraph::algo::tarjan_scc;
use petgraph::graph::{DiGraph, NodeIndex};

use crate::collector::{self, Module};

pub fn compute_topological_sccs(graph: &DiGraph<PathBuf, ()>) -> Vec<Vec<NodeIndex>> {
    tarjan_scc(graph)
}

pub fn is_cyclic_scc(graph: &DiGraph<PathBuf, ()>, nodes: &[NodeIndex]) -> bool {
    if nodes.len() > 1 {
        return true;
    }
    if nodes.len() == 1 {
        let node = nodes[0];
        if graph.find_edge(node, node).is_some() {
            return true;
        }
    }
    false
}

pub fn build_dependency_graph(
    modules: &[Module],
) -> Result<(DiGraph<PathBuf, ()>, HashMap<PathBuf, NodeIndex>)> {
    let mut graph = DiGraph::<PathBuf, ()>::with_capacity(modules.len(), 0);
    let mut node_map = HashMap::with_capacity(modules.len());

    for module in modules {
        let idx = graph.add_node(module.file_path.clone());
        node_map.insert(module.file_path.clone(), idx);
    }

    for module in modules {
        let from_idx = node_map[&module.file_path];
        let deps = collector::read_dependencies(&module.deps_path)?;

        for dep_path in deps {
            if let Some(&to_idx) = node_map.get(&dep_path) {
                graph.add_edge(from_idx, to_idx, ());
            } else {
                log::debug!("Ignoring external dependency: {:?}", dep_path);
            }
        }
    }

    Ok((graph, node_map))
}
