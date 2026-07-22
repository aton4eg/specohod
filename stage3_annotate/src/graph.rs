use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use log::info;
use petgraph::Direction;
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
        for neighbor in graph.neighbors_directed(node, Direction::Outgoing) {
            if neighbor == node {
                return true;
            }
        }
    }
    false
}

pub fn build_dependency_graph(
    modules: &[Module],
) -> Result<(DiGraph<PathBuf, ()>, HashMap<PathBuf, NodeIndex>)> {
    let mut graph = DiGraph::<PathBuf, ()>::new();
    let mut node_map = HashMap::new();

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

#[allow(unused)]
pub fn topological_sort(
    graph: &DiGraph<PathBuf, ()>,
    _node_map: &HashMap<PathBuf, NodeIndex>,
) -> Result<Vec<NodeIndex>> {
    petgraph::algo::toposort(graph, None).map_err(|cycle| {
        anyhow!(
            "Cycle detected involving module: {:?}",
            graph.node_weight(cycle.node_id())
        )
    })
}

#[allow(unused)]
pub fn find_and_print_cycles(graph: &DiGraph<PathBuf, ()>) -> Vec<Vec<PathBuf>> {
    let sccs = tarjan_scc(graph);
    let mut cyclic_components = Vec::new();

    for component in sccs {
        let is_cyclic = if component.len() > 1 {
            true
        } else {
            let node = component[0];
            graph
                .neighbors_directed(node, Direction::Outgoing)
                .any(|n| n == node)
        };

        if is_cyclic {
            let nodes_data: Vec<PathBuf> = component
                .iter()
                .map(|&idx| graph.node_weight(idx).unwrap().clone())
                .collect();
            info!("Cycle detected in SCC with nodes: {:?}", nodes_data);
            cyclic_components.push(nodes_data);
        }
    }

    if cyclic_components.is_empty() {
        info!("No cycles found in the graph");
    }
    cyclic_components
}
