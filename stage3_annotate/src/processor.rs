use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use futures::future::try_join_all;
use petgraph::graph::DiGraph;
use petgraph::graph::NodeIndex;
use petgraph::Direction;
use tokio::sync::mpsc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::collector::Module;
use crate::llm::{Config, LlmClient, LlmStats};

mod generator;

pub struct Prompts {
    pub system: String,
    pub final_spec: String,
    pub prelim_spec: String,
    pub partial: String,
    pub merge: String,
    pub spec_template: String,
}

struct GenerationContext {
    prompts: Arc<Prompts>,
    llm: Arc<LlmClient>,
}

async fn process_scc(
    scc_nodes: &[NodeIndex],
    modules: &[Module],
    graph: &DiGraph<PathBuf, ()>,
    ctx: &GenerationContext,
) -> Result<()> {
    let scc_modules: Vec<&Module> = scc_nodes
        .iter()
        .map(|&idx| {
            let path = graph.node_weight(idx).unwrap();
            modules.iter().find(|m| &m.file_path == path).unwrap()
        })
        .collect();

    let is_cyclic = crate::graph::is_cyclic_scc(graph, scc_nodes);

    if !is_cyclic {
        let module = scc_modules[0];
        generator::generate_spec(module, modules, &module.spec_path, ctx).await?;
        log::info!("Generated final spec for {:?}", module.file_path);
        return Ok(());
    }

    log::info!("Processing cyclic SCC with {} modules", scc_modules.len());

    let mut ordered_modules: Vec<(&Module, usize)> = Vec::new();
    for m in scc_modules {
        let count = generator::count_horizontal_dependencies(m, modules)?;
        ordered_modules.push((m, count));
    }
    ordered_modules.sort_by_key(|(_, cnt)| *cnt);

    // Phase 1: Generate preliminary specs for all modules (parallel)
    try_join_all(ordered_modules.iter().map(|(module, _)| {
        generator::generate_spec(module, modules, &module.prelim_spec_path, ctx)
    }))
    .await?;

    // Phase 2: Generate final specs for all modules (parallel)
    try_join_all(ordered_modules.iter().map(|(module, _)| {
        generator::generate_spec(module, modules, &module.spec_path, ctx)
    }))
    .await?;

    Ok(())
}

/// Build SCC dependency data for the parallel scheduler.
///
/// Returns `(dep_count, dependants)` where:
/// - `dep_count[i]` = number of SCCs that SCC `i` depends on
/// - `dependants[j]` = list of SCCs that depend on SCC `j`
fn build_scc_deps(
    sccs: &[Vec<NodeIndex>],
    graph: &DiGraph<PathBuf, ()>,
) -> (Vec<usize>, Vec<Vec<usize>>) {
    let total = sccs.len();
    if total == 0 {
        return (vec![], vec![]);
    }

    // Map each node to its SCC index
    let mut scc_of_node: HashMap<NodeIndex, usize> = HashMap::new();
    for (idx, scc) in sccs.iter().enumerate() {
        for &node in scc {
            scc_of_node.insert(node, idx);
        }
    }

    let mut dep_count = vec![0usize; total];
    let mut dependants: Vec<Vec<usize>> = vec![vec![]; total];

    for (i, scc) in sccs.iter().enumerate() {
        let mut seen = HashSet::new();
        for &node in scc {
            for neighbor in graph.neighbors_directed(node, Direction::Outgoing) {
                if let Some(&j) = scc_of_node.get(&neighbor)
                    && j != i && seen.insert(j) {
                        dep_count[i] += 1;
                        dependants[j].push(i);
                    }
            }
        }
    }

    (dep_count, dependants)
}

pub async fn process_modules(
    graph: Arc<DiGraph<PathBuf, ()>>,
    modules: Arc<[Module]>,
    prompts: Prompts,
    llm_config: &Config,
    stats: Arc<LlmStats>,
    concurrency: usize,
) -> Result<()> {
    let concurrency = concurrency.max(1);
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let llm = Arc::new(LlmClient::new(llm_config.clone(), stats, semaphore.clone()));
    let ctx = Arc::new(GenerationContext {
        prompts: Arc::new(prompts),
        llm,
    });

    let sccs = crate::graph::compute_topological_sccs(&graph);
    let total = sccs.len();
    if total == 0 {
        return Ok(());
    }

    let (mut dep_count, dependants) = build_scc_deps(&sccs, &graph);

    // Channel for ready SCC indices (in-degree 0 = all deps satisfied)
    let (ready_tx, mut ready_rx) = mpsc::unbounded_channel::<usize>();
    for (i, &count) in dep_count.iter().enumerate() {
        if count == 0 {
            let _ = ready_tx.send(i);
        }
    }

    let mut join_set: JoinSet<(usize, Result<()>)> = JoinSet::new();
    let mut completed = 0;

    loop {
        // Dispatch as many ready SCCs as permits allow
        loop {
            let scc_idx = match ready_rx.try_recv() {
                Ok(idx) => idx,
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => break,
            };

            let permit = match semaphore.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    // All permits in use — push back and wait for a completion
                    let _ = ready_tx.send(scc_idx);
                    break;
                }
            };

            let ctx = ctx.clone();
            let modules = modules.clone();
            let graph = graph.clone();
            let scc = sccs[scc_idx].clone();

            join_set.spawn(async move {
                let _permit = permit;
                let modules: &[Module] = &modules;
                let graph: &DiGraph<PathBuf, ()> = &graph;
                let ctx: &GenerationContext = &ctx;
                let result = process_scc(&scc, modules, graph, ctx).await;
                (scc_idx, result)
            });
        }

        // Wait for one task to complete
        if join_set.is_empty() {
            // All tasks finished without reaching `completed == total`
            // (should not happen if the channel wiring is correct)
            break;
        }

        let Some(result) = join_set.join_next().await else {
            break;
        };

        match result {
            Ok((scc_idx, Ok(()))) => {
                completed += 1;

                // Unblock dependants of this SCC
                for &dep_idx in &dependants[scc_idx] {
                    dep_count[dep_idx] -= 1;
                    if dep_count[dep_idx] == 0 {
                        let _ = ready_tx.send(dep_idx);
                    }
                }
            }
            Ok((_, Err(e))) => {
                // Cancel remaining tasks by dropping the set
                join_set.shutdown().await;
                return Err(e);
            }
            Err(e) => {
                join_set.shutdown().await;
                return Err(anyhow::anyhow!("Task join error: {}", e));
            }
        }

        if completed == total {
            break;
        }
    }

    Ok(())
}