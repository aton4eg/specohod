use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::future::try_join_all;
use log::info;
use petgraph::graph::DiGraph;
use petgraph::graph::NodeIndex;
use petgraph::Direction;
use tokio::sync::mpsc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::collector::Module;
use crate::hash::{compute_hash, write_spec_with_hash};
use crate::llm::{Config, LlmClient, LlmError, LlmStats};
use crate::template;

struct DepData {
    path: PathBuf,
    spec: String,
    facts: String,
}

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

struct PartialSpecContext<'a> {
    code: &'a str,
    facts: &'a str,
    base_path: &'a Path,
    generation: &'a GenerationContext,
}

async fn generate_partial_specs_recursive(
    deps: &[DepData],
    ctx: &PartialSpecContext<'_>,
    start_index: &mut u32,
) -> Result<Vec<PathBuf>> {
    let mut deps_specs = String::new();
    let mut deps_facts = String::new();
    for d in deps {
        deps_specs.push_str(&format!(
            "\n--- Dependency spec: {:?} ---\n{}\n",
            d.path, d.spec
        ));
        deps_facts.push_str(&format!(
            "\n--- Dependency facts: {:?} ---\n{}\n",
            d.path, d.facts
        ));
    }

    let mut vars = HashMap::new();
    vars.insert("module_code", ctx.code.to_string());
    vars.insert("module_pub_interfaces", ctx.facts.to_string());
    vars.insert("specifications_of_module_dependencies", deps_specs);
    vars.insert("public_interfaces_of_dependencies", deps_facts);
    vars.insert(
        "specification_template",
        ctx.generation.prompts.spec_template.to_string(),
    );
    let user_prompt = template::render(&ctx.generation.prompts.partial, &vars);

    match ctx
        .generation
        .llm
        .generate(&ctx.generation.prompts.system, &user_prompt)
        .await
    {
        Ok(generated) => {
            let index = *start_index;
            *start_index += 1;
            let mut partial_path = ctx.base_path.to_path_buf();
            let stem = ctx
                .base_path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            let new_name = format!("{}.partial.{}.spec.md", stem, index);
            partial_path.set_file_name(&new_name);

            std::fs::write(&partial_path, generated)
                .with_context(|| format!("Failed to write partial spec to {:?}", partial_path))?;
            log::info!("Generated partial spec: {:?}", partial_path);
            Ok(vec![partial_path])
        }
        Err(LlmError::ContextLengthExceeded) => {
            if deps.len() <= 1 {
                return Err(anyhow::anyhow!(
                    "Context length exceeded even for a single dependency + module code"
                ));
            }
            let mid = deps.len() / 2;
            let left = &deps[..mid];
            let right = &deps[mid..];

            let mut left_paths =
                Box::pin(generate_partial_specs_recursive(left, ctx, start_index)).await?;
            let right_paths =
                Box::pin(generate_partial_specs_recursive(right, ctx, start_index)).await?;

            left_paths.extend(right_paths);
            Ok(left_paths)
        }
        Err(other) => Err(other.into()),
    }
}

async fn generate_via_partial_merge(
    module: &Module,
    all_modules: &[Module],
    output_path: &Path,
    code: &str,
    facts: &str,
    ctx: &GenerationContext,
    code_hash: &str,
) -> Result<()> {
    let deps = crate::collector::read_dependencies(&module.deps_path)?;

    let mut dep_data_list = Vec::new();
    for dep_path in deps {
        if let Some(dep_module) = all_modules.iter().find(|m| m.file_path == dep_path) {
            let facts = std::fs::read_to_string(&dep_module.facts_path).ok();
            let spec = if dep_module.spec_path.exists() {
                std::fs::read_to_string(&dep_module.spec_path).ok()
            } else if dep_module.prelim_spec_path.exists() {
                std::fs::read_to_string(&dep_module.prelim_spec_path).ok()
            } else {
                log::warn!("No spec found for dependency {:?}", dep_path);
                None
            };

            if let (Some(spec), Some(facts)) = (spec, facts) {
                dep_data_list.push(DepData {
                    path: dep_path,
                    spec,
                    facts,
                });
            }
        }
    }

    let mut start_index = 1u32;
    let partial_ctx = PartialSpecContext {
        code,
        facts,
        base_path: output_path,
        generation: ctx,
    };
    let partial_paths =
        generate_partial_specs_recursive(&dep_data_list, &partial_ctx, &mut start_index).await?;

    let mut partial_specs_content = String::new();
    for p in &partial_paths {
        let content = std::fs::read_to_string(p)
            .with_context(|| format!("Failed to read partial spec {:?}", p))?;
        partial_specs_content.push_str(&format!("\n--- Partial spec: {:?} ---\n{}\n", p, content));
    }

    let mut vars = HashMap::new();
    vars.insert("module_code", code.to_string());
    vars.insert("partial_specifications", partial_specs_content);
    vars.insert("module_pub_interfaces", facts.to_string());
    vars.insert(
        "specification_template",
        ctx.prompts.spec_template.to_string(),
    );
    let user_prompt = template::render(&ctx.prompts.merge, &vars);

    let merged = ctx.llm.generate(&ctx.prompts.system, &user_prompt).await?;

    write_spec_with_hash(output_path, &merged, code_hash)?;
    log::info!("Merged spec written to {:?}", output_path);

    Ok(())
}

async fn generate_spec(
    module: &Module,
    all_modules: &[Module],
    output_path: &Path,
    ctx: &GenerationContext,
) -> Result<()> {
    info!(
        "start generation for {}",
        module.file_path.as_os_str().display()
    );
    if output_path.exists() {
        info!("skip generation, spec exists");
        return Ok(());
    }

    let code = std::fs::read_to_string(&module.file_path)
        .with_context(|| format!("Failed to read {:?}", module.file_path))?;
    let facts = std::fs::read_to_string(&module.facts_path)
        .with_context(|| format!("Failed to read {:?}", module.facts_path))?;

    let code_hash = compute_hash(&code);

    let deps = crate::collector::read_dependencies(&module.deps_path)?;

    let mut deps_specs = String::new();
    let mut deps_facts = String::new();
    let mut prelim_flag = false;

    for dep_path in deps {
        if let Some(dep_module) = all_modules.iter().find(|m| m.file_path == dep_path) {
            if let Ok(f) = std::fs::read_to_string(&dep_module.facts_path) {
                deps_facts.push_str(&format!(
                    "\n--- Dependency facts: {:?} ---\n{}\n",
                    dep_path, f
                ));
            }

            let spec_content = if dep_module.spec_path.exists() {
                std::fs::read_to_string(&dep_module.spec_path).ok()
            } else if dep_module.prelim_spec_path.exists() {
                std::fs::read_to_string(&dep_module.prelim_spec_path).ok()
            } else {
                prelim_flag = true;
                log::warn!("No spec found for dependency {:?}", dep_path);
                None
            };

            if let Some(spec) = spec_content {
                deps_specs.push_str(&format!(
                    "\n--- Dependency spec: {:?} ---\n{}\n",
                    dep_path, spec
                ));
            }
        }
    }

    let mut vars = HashMap::new();
    vars.insert("module_code", code.clone());
    vars.insert("module_pub_interfaces", facts.clone());
    vars.insert("specifications_of_module_dependencies", deps_specs);
    vars.insert("public_interfaces_of_dependencies", deps_facts);
    vars.insert(
        "specification_template",
        ctx.prompts.spec_template.to_string(),
    );

    let user_prompt = if prelim_flag {
        template::render(&ctx.prompts.prelim_spec, &vars)
    } else {
        template::render(&ctx.prompts.final_spec, &vars)
    };

    match ctx.llm.generate(&ctx.prompts.system, &user_prompt).await {
        Ok(generated) => {
            write_spec_with_hash(output_path, &generated, &code_hash)?;
            info!(
                "ENDED generation for {}",
                module.file_path.as_os_str().display()
            );
            Ok(())
        }
        Err(LlmError::ContextLengthExceeded) => {
            log::warn!(
                "Context length exceeded for module {:?}, switching to partial generation",
                module.file_path
            );
            generate_via_partial_merge(module, all_modules, output_path, &code, &facts, ctx, &code_hash)
                .await
        }
        Err(other) => Err(other.into()),
    }
}

fn count_horizontal_deps(module: &Module, all_modules: &[Module]) -> Result<usize> {
    let deps = crate::collector::read_dependencies(&module.deps_path)?;
    let current_parent = module.file_path.parent();

    let mut count = 0;
    for dep_path in deps {
        if let Some(dep_module) = all_modules.iter().find(|m| m.file_path == dep_path)
            && dep_module.file_path.parent() == current_parent
        {
            count += 1;
        }
    }
    Ok(count)
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
        generate_spec(module, modules, &module.spec_path, ctx).await?;
        log::info!("Generated final spec for {:?}", module.file_path);
        return Ok(());
    }

    log::info!("Processing cyclic SCC with {} modules", scc_modules.len());

    let mut ordered_modules: Vec<(&Module, usize)> = Vec::new();
    for m in scc_modules {
        let count = count_horizontal_deps(m, modules)?;
        ordered_modules.push((m, count));
    }
    ordered_modules.sort_by_key(|(_, cnt)| *cnt);

    // Phase 1: Generate preliminary specs for all modules (parallel)
    try_join_all(ordered_modules.iter().map(|(module, _)| {
        generate_spec(module, modules, &module.prelim_spec_path, ctx)
    }))
    .await?;

    // Phase 2: Generate final specs for all modules (parallel)
    try_join_all(ordered_modules.iter().map(|(module, _)| {
        generate_spec(module, modules, &module.spec_path, ctx)
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
                if let Some(&j) = scc_of_node.get(&neighbor) {
                    if j != i && seen.insert(j) {
                        dep_count[i] += 1;
                        dependants[j].push(i);
                    }
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

    let sccs = crate::graph::topological_sccs(&graph);
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
                let modules: &[Module] = &*modules;
                let graph: &DiGraph<PathBuf, ()> = &*graph;
                let ctx: &GenerationContext = &*ctx;
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