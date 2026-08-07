use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use log::info;

use crate::collector::Module;
use crate::hash::{compute_hash, write_spec_with_hash};
use crate::llm::LlmError;
use crate::template;

use super::GenerationContext;

pub(super) struct DepData {
    pub(super) path: PathBuf,
    pub(super) spec: String,
    pub(super) facts: String,
}

fn format_dep_sections(deps: &[DepData]) -> (String, String) {
    let mut specs = String::new();
    let mut facts = String::new();
    for d in deps {
        specs.push_str(&format!(
            "\n--- Dependency spec: {:?} ---\n{}\n",
            d.path, d.spec
        ));
        facts.push_str(&format!(
            "\n--- Dependency facts: {:?} ---\n{}\n",
            d.path, d.facts
        ));
    }
    (specs, facts)
}

/// A generated partial specification kept in memory (path is retained only for
/// the on-disk artifact and for logging).
struct PartialSpec {
    path: PathBuf,
    content: String,
}

struct PartialSpecContext<'a> {
    code: &'a str,
    facts: &'a str,
    base_path: &'a Path,
    generation: &'a GenerationContext,
}

/// Loads dependency data (facts and spec text) for all direct dependencies of
/// a module. Falls back to preliminary specs if final specs are not yet
/// available.
///
/// # Returns
///
/// A tuple of `(dep_data, prelim_flag)` where `prelim_flag` is `true` if any
/// dependency only had a preliminary (not final) spec.
pub(super) fn load_dep_data(
    module: &Module,
    all_modules: &[Module],
    ctx: &GenerationContext,
) -> (Vec<DepData>, bool) {
    let Ok(deps) = crate::collector::read_dependencies(&module.deps_path) else {
        return (Vec::new(), false);
    };
    let mut dep_data = Vec::with_capacity(deps.len());
    let mut prelim_flag = false;
    for dep_path in deps {
        let Some(dep_module) = ctx.module_index.get(&dep_path).map(|&i| &all_modules[i]) else {
            continue;
        };
        let Ok(facts) = std::fs::read_to_string(&dep_module.facts_path) else {
            continue;
        };
        let spec = if dep_module.spec_path.exists() {
            std::fs::read_to_string(&dep_module.spec_path).ok()
        } else if dep_module.prelim_spec_path.exists() {
            std::fs::read_to_string(&dep_module.prelim_spec_path).ok()
        } else {
            prelim_flag = true;
            log::warn!("No spec found for dependency {:?}", dep_path);
            continue;
        };
        dep_data.push(DepData {
            path: dep_path,
            spec: spec.unwrap(),
            facts,
        });
    }
    (dep_data, prelim_flag)
}

async fn generate_partial_specs_recursive(
    deps: &[DepData],
    ctx: &PartialSpecContext<'_>,
    start_index: &mut u32,
) -> Result<Vec<PartialSpec>> {
    let (deps_specs, deps_facts) = format_dep_sections(deps);

    let mut vars = HashMap::with_capacity(5);
    vars.insert("module_code", ctx.code.to_string());
    vars.insert("module_pub_interfaces", ctx.facts.to_string());
    vars.insert("specifications_of_module_dependencies", deps_specs);
    vars.insert("public_interfaces_of_dependencies", deps_facts);
    vars.insert(
        "specification_template",
        ctx.generation.prompts.spec_template.to_string(),
    );
    let user_prompt = template::render_template(&ctx.generation.prompts.partial, &vars);

    match ctx
        .generation
        .llm
        .generate_completion(&ctx.generation.prompts.system, &user_prompt)
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

            std::fs::write(&partial_path, &generated)
                .with_context(|| format!("Failed to write partial spec to {:?}", partial_path))?;
            log::info!("Generated partial spec: {:?}", partial_path);
            Ok(vec![PartialSpec {
                path: partial_path,
                content: generated,
            }])
        }
        Err(LlmError::ContextLengthExceeded) => {
            if deps.len() <= 1 {
                return Err(anyhow::anyhow!(
                    "Context length exceeded even for a single dependency. \
                     The dependency facts may be too large for the model's context window. \
                     Consider increasing --context-length or using a model with larger context."
                ));
            }
            let mid = deps.len() / 2;
            let left = &deps[..mid];
            let right = &deps[mid..];

            let mut left_specs =
                Box::pin(generate_partial_specs_recursive(left, ctx, start_index)).await?;
            let right_specs =
                Box::pin(generate_partial_specs_recursive(right, ctx, start_index)).await?;

            left_specs.extend(right_specs);
            Ok(left_specs)
        }
        Err(other) => Err(other.into()),
    }
}

async fn generate_via_partial_merge(
    output_path: &Path,
    code: &str,
    facts: &str,
    code_hash: &str,
    dep_data_list: &[DepData],
    ctx: &GenerationContext,
) -> Result<()> {
    let mut start_index = 1u32;
    let partial_ctx = PartialSpecContext {
        code,
        facts,
        base_path: output_path,
        generation: ctx,
    };
    let partial_specs =
        generate_partial_specs_recursive(dep_data_list, &partial_ctx, &mut start_index).await?;

    let mut partial_specs_content = String::new();
    for p in &partial_specs {
        partial_specs_content.push_str(&format!(
            "\n--- Partial spec: {:?} ---\n{}\n",
            p.path, p.content
        ));
    }

    let mut vars = HashMap::with_capacity(4);
    vars.insert("module_code", code.to_string());
    vars.insert("partial_specifications", partial_specs_content);
    vars.insert("module_pub_interfaces", facts.to_string());
    vars.insert(
        "specification_template",
        ctx.prompts.spec_template.to_string(),
    );
    let user_prompt = template::render_template(&ctx.prompts.merge, &vars);

    let merged = ctx
        .llm
        .generate_completion(&ctx.prompts.system, &user_prompt)
        .await?;

    write_spec_with_hash(output_path, &merged, code_hash)?;
    log::info!("Merged spec written to {:?}", output_path);

    Ok(())
}

/// Generates a specification for a single module.
///
/// If the output file already exists, it is skipped. If the context length is
/// exceeded, falls back to partial generation and merge.
///
/// # Parameters
///
/// * `module` — The module to generate a spec for.
/// * `all_modules` — All workspace modules.
/// * `output_path` — Where to write the generated spec.
/// * `ctx` — Shared generation context with prompts and LLM client.
pub(super) async fn generate_spec(
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

    let (dep_data, prelim_flag) = load_dep_data(module, all_modules, ctx);

    let (deps_specs, deps_facts) = format_dep_sections(&dep_data);

    let mut vars = HashMap::with_capacity(5);
    vars.insert("module_code", code.clone());
    vars.insert("module_pub_interfaces", facts.clone());
    vars.insert("specifications_of_module_dependencies", deps_specs);
    vars.insert("public_interfaces_of_dependencies", deps_facts);
    vars.insert(
        "specification_template",
        ctx.prompts.spec_template.to_string(),
    );

    let user_prompt = if prelim_flag {
        template::render_template(&ctx.prompts.prelim_spec, &vars)
    } else {
        template::render_template(&ctx.prompts.final_spec, &vars)
    };

    match ctx
        .llm
        .generate_completion(&ctx.prompts.system, &user_prompt)
        .await
    {
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
            generate_via_partial_merge(output_path, &code, &facts, &code_hash, &dep_data, ctx).await
        }
        Err(other) => Err(other.into()),
    }
}
