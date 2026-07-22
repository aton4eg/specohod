use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use log::info;
use petgraph::graph::DiGraph;
use petgraph::graph::NodeIndex;

use crate::collector::Module;
use crate::hash::{compute_hash, write_spec_with_hash};
use crate::llm::{Config, LlmClient, LlmError, LlmStats};
use crate::template;

struct DepData {
    path: PathBuf,
    spec: String,
    facts: String,
}

pub struct Prompts<'a> {
    pub system: &'a str,
    pub final_spec: &'a str,
    pub prelim_spec: &'a str,
    pub partial: &'a str,
    pub merge: &'a str,
    pub spec_template: &'a str,
}

struct GenerationContext<'a> {
    prompts: Prompts<'a>,
    llm: &'a LlmClient,
}

struct PartialSpecContext<'a> {
    code: &'a str,
    facts: &'a str,
    base_path: &'a Path,
    generation: &'a GenerationContext<'a>,
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
    let user_prompt = template::render(ctx.generation.prompts.partial, &vars);

    match ctx
        .generation
        .llm
        .generate(ctx.generation.prompts.system, &user_prompt)
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
    ctx: &GenerationContext<'_>,
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
    let user_prompt = template::render(ctx.prompts.merge, &vars);

    let merged = ctx.llm.generate(ctx.prompts.system, &user_prompt).await?;

    write_spec_with_hash(output_path, &merged, code_hash)?;
    log::info!("Merged spec written to {:?}", output_path);

    Ok(())
}

async fn generate_spec(
    module: &Module,
    all_modules: &[Module],
    output_path: &Path,
    ctx: &GenerationContext<'_>,
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
        template::render(ctx.prompts.prelim_spec, &vars)
    } else {
        template::render(ctx.prompts.final_spec, &vars)
    };

    match ctx.llm.generate(ctx.prompts.system, &user_prompt).await {
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
            generate_via_partial_merge(
                module,
                all_modules,
                output_path,
                &code,
                &facts,
                ctx,
                &code_hash,
            )
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
    ctx: &GenerationContext<'_>,
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

    for (module, _) in &ordered_modules {
        generate_spec(module, modules, &module.prelim_spec_path, ctx).await?;
        log::info!("Generated preliminary spec for {:?}", module.file_path);
    }

    for (module, _) in &ordered_modules {
        generate_spec(module, modules, &module.spec_path, ctx).await?;
        log::info!("Generated final spec for {:?}", module.file_path);
    }

    Ok(())
}

pub async fn process_modules(
    graph: &DiGraph<PathBuf, ()>,
    modules: &[Module],
    prompts: Prompts<'_>,
    llm_config: &Config,
    stats: Arc<LlmStats>,
) -> Result<()> {
    let llm_client = Arc::new(LlmClient::new(llm_config.clone(), stats.clone()));
    let generation_ctx = GenerationContext {
        prompts,
        llm: &llm_client,
    };
    let sccs = crate::graph::topological_sccs(graph);

    for scc in sccs {
        process_scc(&scc, modules, graph, &generation_ctx).await?;
    }

    Ok(())
}
