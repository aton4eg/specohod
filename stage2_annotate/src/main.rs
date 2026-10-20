use std::{
    collections::{HashMap, HashSet},
    fs::exists,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use clap::Parser;
use llm::LlmStats;
use log::info;
use petgraph::Direction;
use petgraph::graph::NodeIndex;
use sha2::{Digest, Sha256};
use std::sync::atomic::Ordering;

// ============================================================
// NEW: economy state shared across processor
// ============================================================
pub struct EconomyState {
    pub update_prompt: String,
    pub dirty: Mutex<HashSet<PathBuf>>,
}

impl EconomyState {
    pub fn new(update_prompt: String, changed: HashSet<PathBuf>) -> Self {
        Self {
            update_prompt,
            dirty: Mutex::new(HashSet::new()),
        }
    }

    pub fn is_dirty(&self, path: &std::path::Path) -> bool {
        self.dirty.lock().unwrap().contains(path)
    }
    pub fn mark_dirty(&self, path: &std::path::Path) {
        self.dirty.lock().unwrap().insert(path.to_path_buf());
    }
    pub fn any_dep_affected(&self, deps: &[PathBuf]) -> bool {
        let d = self.dirty.lock().unwrap();
        deps.iter().any(|p| d.contains(p))
    }
}

mod processor {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use anyhow::{Context, Result};
    use log::info;
    use petgraph::graph::DiGraph;
    use petgraph::graph::NodeIndex;

    use crate::collector::Module;
    use crate::llm::{Config, LlmClient, LlmError, LlmStats};
    use crate::template;

    // Hashing utilities (defined above in main.rs, but used here too)
    use crate::{EconomyState, compute_hash, write_spec_with_hash};

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
        economy: Option<&'a EconomyState>,
    }

    struct PartialSpecContext<'a> {
        code: &'a str,
        facts: &'a str,
        base_path: &'a Path,
        generation: &'a GenerationContext<'a>,
    }

    // ---------- helpers ----------
    /// Strip the leading hash comment from a spec body (for update).
    fn strip_hash_line(spec: &str) -> String {
        let mut lines = spec.lines();
        if let Some(first) = lines.next() {
            if first.trim_start().starts_with("<!-- SPEC_HASH:") {
                return lines.collect::<Vec<_>>().join("\n");
            }
        }
        spec.to_string()
    }

    /// Recursively generates partial specs, splitting dependencies in half on context overflow.
    /// `deps` – list of dependencies to be covered by this partial spec.
    /// `base_path` – path to final spec file (e.g., foo.spec.md), used to form partial file names.
    /// `start_index` – counter for numbering partial files (modified in place).
    /// Returns paths to all created partial files.
    async fn generate_partial_specs_recursive(
        deps: &[DepData],
        ctx: &PartialSpecContext<'_>,
        start_index: &mut u32,
    ) -> Result<Vec<PathBuf>> {
        // Build specs and facts text for current group
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
        // We don't use template in partial prompt, but can add if needed
        let user_prompt = template::render(ctx.generation.prompts.partial, &vars);

        // Try to generate one partial spec
        match ctx
            .generation
            .llm
            .generate(ctx.generation.prompts.system, &user_prompt)
            .await
        {
            Ok(generated) => {
                let index = *start_index;
                *start_index += 1;
                // Simplify: use base_path, replacing extension
                let mut partial_path = ctx.base_path.to_path_buf();
                let stem = ctx
                    .base_path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy();
                // For foo.spec.md -> foo_partial1.spec.md
                // For foo.preliminary.spec.md -> foo.preliminary_partial1.spec.md
                let new_name = format!("{}.partial.{}.spec.md", stem, index);
                partial_path.set_file_name(&new_name);

                std::fs::write(&partial_path, generated).with_context(|| {
                    format!("Failed to write partial spec to {:?}", partial_path)
                })?;
                log::info!("Generated partial spec: {:?}", partial_path);
                Ok(vec![partial_path])
            }
            Err(LlmError::ContextLengthExceeded) => {
                // Split the group in half
                if deps.len() <= 1 {
                    // By task condition this situation should not occur,
                    // but if it does, return error.
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

    /// Generates specification using partial specs and merge strategy.
    async fn generate_via_partial_merge(
        module: &Module,
        all_modules: &[Module],
        output_path: &Path,
        code: &str,
        facts: &str,
        ctx: &GenerationContext<'_>,
        code_hash: &str, // <-- new parameter
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
                        spec: strip_hash_line(&spec),
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
            generate_partial_specs_recursive(&dep_data_list, &partial_ctx, &mut start_index)
                .await?;

        let mut partial_specs_content = String::new();
        for p in &partial_paths {
            let content = std::fs::read_to_string(p)
                .with_context(|| format!("Failed to read partial spec {:?}", p))?;
            partial_specs_content.push_str(&format!(
                "\n--- Partial spec: {:?} ---\n{}\n",
                p,
                strip_hash_line(&content)
            ));
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

        // Insert hash and write
        write_spec_with_hash(output_path, &merged, code_hash)?;
        log::info!("Merged spec written to {:?}", output_path);

        for path in partial_paths {
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("Failed to remove {:?}", path))?;
                info!("Removed partial spec: {:?}", path);
            }
        }

        Ok(())
    }

    /// Generates specification for module and saves to `output_path`.
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

        // Calculate source code hash
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
                        dep_path,
                        strip_hash_line(&spec)
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
                // Write with hash
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

    // ---------- compare-specs ----------

    /// Ask LLM whether two spec bodies are semantically equivalent.
    /// Returns true for "yes", false for "no".
    /// Retries up to 10 times on ambiguous answers. Bails out after 10 failures.
    async fn update_specs(
        ctx: &GenerationContext<'_>,
        old_spec: &str,
        dep_spec: &str,
    ) -> Option<String> {
        let economy = ctx.economy.unwrap();

        let mut vars = HashMap::new();
        vars.insert("old_spec", old_spec.to_string());
        vars.insert("dep_spec", dep_spec.to_string());

        let mut last_response = String::new();
        for attempt in 1..=10 {
            let user_prompt = template::render(&economy.update_prompt, &vars);
            match ctx.llm.generate(ctx.prompts.system, &user_prompt).await {
                Ok(response) => {
                    last_response = response.clone();
                    let last_word = response
                        .trim()
                        .split_whitespace()
                        .last()
                        .unwrap_or("")
                        .trim_matches(|c: char| !c.is_alphabetic())
                        .to_lowercase();

                    if last_word == "no" || last_word == "нет" {
                        return None;
                    }
                    return Some(response);
                }
                Err(e) => {
                    log::warn!("Compare-spec attempt {}/10 failed: {:?}", attempt, e);
                    return None;
                }
            }
        }
        return None;
    }

    // ---------- outcome enum ----------

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SpecOutcome {
        Skipped,
        Unchanged,
        Changed,
    }

    // ---------- economy-aware generation ----------

    /// Generates spec for `output_path` in economy mode.
    /// - module changed -> normal rebuild; dirty.
    /// - module affected (dep dirty) but not changed -> build .new and decide whether it should be propagated further
    /// - module unaffected -> skip.
    async fn generate_spec_with_economy(
        module: &Module,
        modules: &[Module],
        ctx: &GenerationContext<'_>,
        output_path_old: &Path,
        output_path_new: &Path,
        prelim: bool,
    ) -> Result<SpecOutcome> {
        info!(
            "start economy generation for {}",
            module.file_path.as_os_str().display()
        );

        if output_path_new.exists() {
            info!("skip generation, new spec exists");
            return Ok(SpecOutcome::Skipped);
        }

        let economy = ctx.economy.as_ref().unwrap();
        let deps = crate::collector::read_dependencies(&module.deps_path)?;
        let dep_affected = economy.any_dep_affected(&deps);
        let missing_spec = !module.spec_path.exists();

        if !dep_affected && !missing_spec {
            info!("Economy: skip (unaffected) {:?}", module.file_path);
            return Ok(SpecOutcome::Skipped);
        }

        let economy = ctx.economy.expect("economy context");

        let code = std::fs::read_to_string(&module.file_path)
            .with_context(|| format!("Failed to read {:?}", module.file_path))?;

        if missing_spec {
            info!("Economy: rebuild (changed/new) {:?}", module.file_path);
            generate_spec(module, modules, &output_path_old, ctx).await?;
            economy.mark_dirty(&module.file_path);
            return Ok(SpecOutcome::Changed);
        }

        // Affected but not changed -> build .new, compare
        info!("Economy: affected (compare) {:?}", module.file_path);

        let old_raw = std::fs::read_to_string(output_path_old)
            .with_context(|| format!("Failed to read {:?}", output_path_old))?;
        let old_body = strip_hash_line(&old_raw);

        let mut dep_specs = String::new();

        let deps = crate::collector::read_dependencies(&module.deps_path)?;
        for dep_path in deps {
            if economy.is_dirty(&dep_path) {
                if let Some(dep_module) = modules.iter().find(|m| m.file_path == dep_path) {
                    let spec_content = if prelim {
                        std::fs::read_to_string(&dep_module.prelim_spec_path).unwrap()
                    } else {
                        std::fs::read_to_string(&dep_module.spec_path).unwrap()
                    };
                    dep_specs += &spec_content;
                }
            }
        }

        let response = update_specs(ctx, &old_body, &dep_specs).await;

        if let Some(spec) = response {
            let code_hash = compute_hash(&code);
            write_spec_with_hash(output_path_new, &spec, &code_hash)?;
            economy.mark_dirty(&module.file_path);
            info!(
                "Economy: spec {:?} will get updates, propagation continues",
                module.file_path
            );
            Ok(SpecOutcome::Changed)
        } else {
            info!(
                "Economy: spec {:?} unchanged, propagation stops",
                module.file_path
            );
            Ok(SpecOutcome::Unchanged)
        }
    }

    /// Counts module's "horizontal" dependencies.
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

    /// Processes one strongly connected component.
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

        // --- non-cyclic ---
        if !is_cyclic {
            let module = scc_modules[0];
            if ctx.economy.is_some() {
                generate_spec_with_economy(
                    module,
                    modules,
                    ctx,
                    &module.spec_path,
                    &module.new_spec_path,
                    false,
                )
                .await?;
            } else {
                generate_spec(module, modules, &module.spec_path, ctx).await?;
                log::info!("Generated final spec for {:?}", module.file_path);
            }
            return Ok(());
        }

        log::info!("Processing cyclic SCC with {} modules", scc_modules.len());

        let mut ordered_modules: Vec<(&Module, usize)> = Vec::new();
        for m in scc_modules {
            let count = count_horizontal_deps(m, modules)?;
            ordered_modules.push((m, count));
        }
        ordered_modules.sort_by_key(|(_, cnt)| *cnt);

        // --- non-economy: legacy flow ---
        if ctx.economy.is_none() {
            // Stage 1: preliminary specifications
            for (module, _) in &ordered_modules {
                generate_spec(module, modules, &module.prelim_spec_path, ctx).await?;
                log::info!("Generated preliminary spec for {:?}", module.file_path);
            }

            // Stage 2: final specifications
            for (module, _) in &ordered_modules {
                generate_spec(module, modules, &module.spec_path, ctx).await?;
                log::info!("Generated final spec for {:?}", module.file_path);
            }
            return Ok(());
        } else {
            for (module, _) in &ordered_modules {
                generate_spec_with_economy(
                    module,
                    modules,
                    ctx,
                    &module.prelim_spec_path,
                    &module.new_prelim_spec_path,
                    true,
                )
                .await?;
            }

            for (module, _) in &ordered_modules {
                generate_spec_with_economy(
                    module,
                    modules,
                    ctx,
                    &module.spec_path,
                    &module.new_spec_path,
                    false,
                )
                .await?;
            }
        }

        Ok(())
    }

    pub async fn process_modules(
        graph: &DiGraph<PathBuf, ()>,
        modules: &[Module],
        prompts: Prompts<'_>,
        llm_config: &Config,
        stats: Arc<LlmStats>,
        economy: Option<&EconomyState>,
    ) -> Result<()> {
        let llm_client = Arc::new(LlmClient::new(llm_config.clone(), stats.clone()));
        let generation_ctx = GenerationContext {
            prompts,
            llm: &llm_client,
            economy,
        };
        let sccs = crate::graph::topological_sccs(graph);

        for scc in sccs {
            process_scc(&scc, modules, graph, &generation_ctx).await?;
        }

        for module in modules {
            if module.new_prelim_spec_path.exists() {
                std::fs::rename(&module.new_prelim_spec_path, &module.prelim_spec_path);
            }
            if module.new_prelim_spec_path.exists() {
                std::fs::rename(&module.new_spec_path, &module.spec_path);
            }
        }

        Ok(())
    }
}

mod llm {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::Duration,
    };

    use anyhow::Context;
    use log::info;
    use reqwest::{Client, StatusCode};
    use serde::{Deserialize, Serialize};
    use tokio::time::sleep;

    #[derive(Debug, Clone)]
    pub struct Config {
        pub api_base: String,
        pub api_key: String,
        pub model: String,
        pub max_tokens: u32,
        pub temperature: f32,
    }

    #[derive(Debug)]
    pub enum LlmError {
        ContextLengthExceeded,
        Other(anyhow::Error),
    }

    impl From<anyhow::Error> for LlmError {
        fn from(e: anyhow::Error) -> Self {
            LlmError::Other(e)
        }
    }

    impl From<LlmError> for anyhow::Error {
        fn from(e: LlmError) -> Self {
            match e {
                LlmError::ContextLengthExceeded => {
                    anyhow::anyhow!("LLM context length exceeded")
                }
                LlmError::Other(err) => err,
            }
        }
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct ChatMessage {
        role: String,
        content: String,
    }

    #[derive(Debug, Serialize)]
    struct ChatRequest {
        model: String,
        messages: Vec<ChatMessage>,
        max_tokens: u32,
        temperature: f32,
    }

    #[derive(Debug, Deserialize)]
    struct ChatChoice {
        message: ChatMessage,
    }

    #[derive(Debug, Deserialize)]
    struct ChatResponse {
        choices: Vec<ChatChoice>,
        usage: Option<Usage>,
    }

    #[derive(Debug, Deserialize)]
    struct Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
    }
    #[derive(Default)]
    pub struct LlmStats {
        pub prompt_tokens: AtomicU64,
        pub completion_tokens: AtomicU64,
        pub total_tokens: AtomicU64,
        pub prompt_chars: AtomicU64,
        pub completion_chars: AtomicU64,
    }

    pub struct LlmClient {
        config: Config,
        client: Client,
        stats: Arc<LlmStats>,
    }

    fn sanitize_prompt(text: &str) -> String {
        text.split(' ')
            .filter(|part| !part.is_empty())
            .collect::<Vec<&str>>()
            .join(" ")
    }

    impl LlmClient {
        pub fn new(config: Config, stats: Arc<LlmStats>) -> Self {
            Self {
                config,
                client: Client::new(),
                stats,
            }
        }

        /// Send a prompt and return the generated text.
        /// Returns `Err(LlmError::ContextLengthExceeded)` if the input is too long.
        pub async fn generate(
            &self,
            system_prompt: &str,
            user_prompt: &str,
        ) -> std::result::Result<String, LlmError> {
            const MAX_RETRIES: u32 = 8;
            const INITIAL_BACKOFF_MS: u64 = 1000;

            let url = format!(
                "{}/chat/completions",
                self.config.api_base.trim_end_matches('/')
            );
            let system_prompt = sanitize_prompt(system_prompt);
            let user_prompt = sanitize_prompt(user_prompt);

            let request = ChatRequest {
                model: self.config.model.clone(),
                messages: vec![
                    ChatMessage {
                        role: "system".to_string(),
                        content: system_prompt.to_string(),
                    },
                    ChatMessage {
                        role: "user".to_string(),
                        content: user_prompt.to_string(),
                    },
                ],
                max_tokens: self.config.max_tokens,
                temperature: self.config.temperature,
            };

            let mut attempt = 0;
            loop {
                attempt += 1;
                let response_result = self
                    .client
                    .post(&url)
                    .header("Authorization", format!("Bearer {}", self.config.api_key))
                    .json(&request)
                    .send()
                    .await;

                match response_result {
                    Ok(response) => {
                        let status = response.status();
                        if let Some(remaining) =
                            response.headers().get("x-ratelimit-remaining-requests")
                        {
                            log::debug!("RateLimit remaining requests: {:?}", remaining);
                        }
                        if let Some(remaining_tokens) =
                            response.headers().get("x-ratelimit-remaining-tokens")
                        {
                            log::debug!("RateLimit remaining tokens: {:?}", remaining_tokens);
                        }

                        if status.is_success() {
                            let response_text = response
                                .text()
                                .await
                                .context("Failed to read response body")
                                .map_err(LlmError::Other)?;
                            match serde_json::from_str::<ChatResponse>(&response_text) {
                                Ok(chat_response) => {
                                    let content = chat_response
                                        .choices
                                        .into_iter()
                                        .next()
                                        .map(|c| c.message.content)
                                        .ok_or_else(|| {
                                            anyhow::anyhow!("No choices in LLM response")
                                        })
                                        .map_err(LlmError::Other)?;
                                    // After parsing ChatResponse, inside Ok(response) block
                                    if let Some(usage) = chat_response.usage {
                                        self.stats.prompt_tokens.fetch_add(
                                            usage.prompt_tokens as u64,
                                            Ordering::Relaxed,
                                        );
                                        self.stats.completion_tokens.fetch_add(
                                            usage.completion_tokens as u64,
                                            Ordering::Relaxed,
                                        );
                                        self.stats.total_tokens.fetch_add(
                                            usage.total_tokens as u64,
                                            Ordering::Relaxed,
                                        );
                                    }
                                    // Count characters (can count as string length in bytes or Unicode characters)
                                    let prompt_len =
                                        (system_prompt.len() + user_prompt.len()) as u64;
                                    let completion_len = content.len() as u64;
                                    self.stats
                                        .prompt_chars
                                        .fetch_add(prompt_len, Ordering::Relaxed);
                                    self.stats
                                        .completion_chars
                                        .fetch_add(completion_len, Ordering::Relaxed);

                                    info!("\n=== LLM Cumulative Statistics ===");
                                    info!(
                                        "Prompt tokens:   {}",
                                        self.stats.prompt_tokens.load(Ordering::Relaxed)
                                    );
                                    info!(
                                        "Completion tokens: {}",
                                        self.stats.completion_tokens.load(Ordering::Relaxed)
                                    );
                                    info!(
                                        "Total tokens:    {}",
                                        self.stats.total_tokens.load(Ordering::Relaxed)
                                    );
                                    info!(
                                        "Prompt chars:    {}",
                                        self.stats.prompt_chars.load(Ordering::Relaxed)
                                    );
                                    info!(
                                        "Completion chars: {}",
                                        self.stats.completion_chars.load(Ordering::Relaxed)
                                    );

                                    return Ok(content);
                                }
                                Err(parse_err) => {
                                    log::warn!(
                                        "Failed to parse LLM response (attempt {}/{}): {}. Raw body: {}",
                                        attempt,
                                        MAX_RETRIES,
                                        parse_err,
                                        &response_text[..response_text.len().min(500)]
                                    );
                                    if attempt < MAX_RETRIES {
                                        let delay = INITIAL_BACKOFF_MS * 2u64.pow(attempt - 1);
                                        log::info!("Retrying after {} ms...", delay);
                                        sleep(Duration::from_millis(delay)).await;
                                        continue;
                                    } else {
                                        return Err(LlmError::Other(anyhow::anyhow!(
                                            "Failed to parse LLM response after {} attempts: {}\nBody: {}",
                                            attempt,
                                            parse_err,
                                            response_text
                                        )));
                                    }
                                }
                            }
                        } else {
                            let status_code = status.as_u16();
                            let error_text = response.text().await.unwrap_or_default();
                            log::warn!(
                                "LLM request failed (attempt {}/{}): status {}, body: {}",
                                attempt,
                                MAX_RETRIES,
                                status_code,
                                error_text
                            );

                            // Check if context limit is exceeded
                            if status_code == 400
                                && (error_text
                                    .to_lowercase()
                                    .contains("exceeds the model's maximum context length")
                                    || error_text
                                        .to_lowercase()
                                        .contains("is longer than the model's context length"))
                            {
                                return Err(LlmError::ContextLengthExceeded);
                            }

                            let should_retry = matches!(
                                status,
                                StatusCode::TOO_MANY_REQUESTS
                                    | StatusCode::INTERNAL_SERVER_ERROR
                                    | StatusCode::BAD_GATEWAY
                                    | StatusCode::SERVICE_UNAVAILABLE
                                    | StatusCode::GATEWAY_TIMEOUT
                            ) && attempt < MAX_RETRIES;

                            if should_retry {
                                let delay = INITIAL_BACKOFF_MS * 2u64.pow(attempt - 1);
                                log::info!("Retrying after {} ms...", delay);
                                sleep(Duration::from_millis(delay)).await;
                                continue;
                            } else {
                                return Err(LlmError::Other(anyhow::anyhow!(
                                    "LLM API error after {} attempts: {} - {}",
                                    attempt,
                                    status,
                                    error_text
                                )));
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!(
                            "LLM request network error (attempt {}/{}): {}",
                            attempt,
                            MAX_RETRIES,
                            e
                        );
                        if attempt < MAX_RETRIES {
                            let delay = INITIAL_BACKOFF_MS * 2u64.pow(attempt - 1);
                            log::info!("Retrying after {} ms...", delay);
                            sleep(Duration::from_millis(delay)).await;
                            continue;
                        } else {
                            return Err(LlmError::Other(
                                anyhow::Error::new(e).context("LLM request failed after retries"),
                            ));
                        }
                    }
                }
            }
        }
    }
}

mod graph {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use anyhow::{Result, anyhow};
    use log::info;
    use petgraph::Direction;
    use petgraph::algo::tarjan_scc;
    use petgraph::graph::{DiGraph, NodeIndex};

    use crate::collector::{self, Module};

    pub fn topological_sccs(graph: &DiGraph<PathBuf, ()>) -> Vec<Vec<NodeIndex>> {
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
}

mod template {
    use std::collections::HashMap;

    pub fn render(template: &str, vars: &HashMap<&str, String>) -> String {
        let mut result = template.to_string();
        for (key, value) in vars {
            let placeholder = format!("{{{{{}}}}}", key);
            result = result.replace(&placeholder, value);
        }
        result
    }
}

mod collector {
    use anyhow::{Context, Result};
    use serde::Deserialize;
    use std::path::{Path, PathBuf};
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
        pub new_spec_path: PathBuf,
        pub new_prelim_spec_path: PathBuf,
    }

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
                let new_spec_path = rs_path.with_extension("spec.new.md");
                let new_prelim_spec_path = rs_path.with_extension("preliminary.spec.new.md");

                if deps_path.exists() && facts_path.exists() {
                    modules.push(Module {
                        file_path: rs_path,
                        deps_path,
                        facts_path,
                        spec_path,
                        prelim_spec_path,
                        new_spec_path,
                        new_prelim_spec_path,
                    });
                }
            }
        }

        Ok(modules)
    }

    pub fn read_dependencies(deps_path: &Path) -> Result<Vec<PathBuf>> {
        let content = std::fs::read_to_string(deps_path)
            .with_context(|| format!("Failed to read {:?}", deps_path))?;
        let deps: DepsFile = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {:?}", deps_path))?;

        let mut paths = Vec::new();
        for dep in deps.dependency {
            let path = PathBuf::from(&dep.file_path);
            if let Ok(canon) = path.canonicalize() {
                paths.push(canon);
            } else {
                log::warn!("Dependency path does not exist: {}", dep.file_path);
            }
        }
        Ok(paths)
    }
}

// ---------- hash utilities ----------
fn compute_hash(contents: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(contents.as_bytes());
    hex::encode(hasher.finalize())
}

/// Attempts to extract hash from the first line of specification.
/// Expected format: `<!-- SPEC_HASH: <hex> -->`
fn extract_hash_from_spec(spec_path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(spec_path).ok()?;
    let first_line = content.lines().next()?;
    let trimmed = first_line.trim();
    trimmed
        .strip_prefix("<!-- SPEC_HASH: ")
        .and_then(|s| s.strip_suffix(" -->"))
        .map(|inner| inner.to_string())
}

/// Writes specification, adding source file hash as first line.
fn write_spec_with_hash(path: &std::path::Path, spec_body: &str, hash: &str) -> Result<()> {
    let full = format!("<!-- SPEC_HASH: {} -->\n{}", hash, spec_body);
    std::fs::write(path, full).with_context(|| format!("Failed to write spec to {:?}", path))?;
    Ok(())
}

// ---------- incremental update ----------

/// Deletes all partial specs created for this module.
fn delete_partial_specs(module: &collector::Module) -> Result<()> {
    let dir = module
        .spec_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));

    // Stems that form partial file names
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

        // Partial files have format: {stem}_partial{number}.spec.md
        if fname.ends_with(".spec.md") {
            for stem in &[&*stem_spec, &*stem_prelim] {
                if fname.starts_with::<&str>(stem.as_ref()) {
                    let rest = &fname[stem.len()..];
                    if rest.starts_with(".partial.") {
                        std::fs::remove_file(&path)
                            .with_context(|| format!("Failed to remove partial spec {:?}", path))?;
                        log::info!("Removed partial spec: {:?}", path);
                        break; // don't try to remove file twice
                    }
                }
            }
        }
    }
    Ok(())
}

/// Compute set of modules whose code hash differs from the hash in the existing spec.
fn compute_changed_set(modules: &[collector::Module]) -> Result<HashSet<PathBuf>> {
    let mut changed = HashSet::new();
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
    Ok(changed)
}

/// Delete final + preliminary specs (and partials) only for the given set.
fn delete_changed_specs(modules: &[collector::Module], changed: &HashSet<PathBuf>) -> Result<()> {
    for module in modules {
        if changed.contains(&module.file_path) {
            if module.spec_path.exists() {
                std::fs::remove_file(&module.spec_path)
                    .with_context(|| format!("Failed to remove {:?}", module.spec_path))?;
            }
            if module.prelim_spec_path.exists() {
                std::fs::remove_file(&module.prelim_spec_path)
                    .with_context(|| format!("Failed to remove {:?}", module.prelim_spec_path))?;
            }
            delete_partial_specs(module)?;
        }
    }
    Ok(())
}

/// Delete final + preliminary specs (and partials) only for the given set.
fn propagate_affected_set(modules: &[collector::Module], economy_state: &EconomyState) {
    for m in modules {
        if m.new_prelim_spec_path.exists() || m.new_spec_path.exists() {
            economy_state.mark_dirty(&m.file_path);
        }
    }
}

/// Legacy non-economy incremental update.
fn incremental_update(
    modules: &[collector::Module],
    graph: &petgraph::graph::DiGraph<PathBuf, ()>,
    node_map: &HashMap<PathBuf, NodeIndex>,
) -> Result<()> {
    let mut changed = HashSet::new();

    // 1. Determine changed modules
    for module in modules {
        let code = std::fs::read_to_string(&module.file_path)
            .with_context(|| format!("Failed to read {:?}", module.file_path))?;
        let current_hash = compute_hash(&code);

        let need_update = if module.spec_path.exists() {
            match extract_hash_from_spec(&module.spec_path) {
                Some(saved_hash) => saved_hash != current_hash,
                None => true, // no hash in spec – consider changed
            }
        } else {
            true // no spec – new module
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

    // 2. Find all modules that (transitively) depend on changed ones
    let mut affected: HashSet<PathBuf> = changed.clone();
    let mut queue: Vec<PathBuf> = changed.iter().cloned().collect();

    while let Some(current) = queue.pop() {
        if let Some(&node) = node_map.get(&current) {
            // all modules that depend on current (incoming edges)
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

    // 3. Delete final and preliminary specs of affected modules,
    //    as well as all their partial specs.
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
            // delete partial specs of this module
            delete_partial_specs(module)?;
        }
    }

    Ok(())
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    root: PathBuf,

    #[arg(short, long, default_value = "prompts/system.txt")]
    system_prompt: PathBuf,

    #[arg(short, long, default_value = "prompts/build_final_spec.txt")]
    final_spec_prompt: PathBuf,

    #[arg(short, long, default_value = "prompts/build_prelim_spec.txt")]
    prelim_spec_prompt: PathBuf,

    #[arg(long, default_value = "prompts/build_partial_spec.txt")]
    partial_prompt: PathBuf,

    #[arg(long, default_value = "prompts/merge_partial_specs.txt")]
    merge_prompt: PathBuf,

    #[arg(short, long, default_value = "prompts/spec_template.txt")]
    spec_template: PathBuf,

    #[arg(long, default_value = "")]
    api_base: String,

    #[arg(long, default_value = "")]
    api_key: String,

    #[arg(long, default_value = "")]
    model: String,

    #[arg(long, default_value_t = 20000)]
    max_tokens: u32,

    #[arg(long, default_value_t = 0.0)]
    temperature: f32,

    #[arg(long, default_value_t = 4)]
    concurrency: usize,

    #[arg(long, default_value_t = 182752)]
    context_length: u32,

    /// Enable incremental updates: regenerate only changed modules and their dependents.
    #[arg(long)]
    incremental: bool,

    // NEW: incremental economy
    /// Enable incremental economy mode: regenerate only changed modules.
    /// Dependents are only rebuilt if their new spec differs from the old one
    /// (checked via --update_spec_prompt).
    #[arg(long)]
    incremental_economy: bool,

    // NEW: prompt used to compare old vs new spec.
    /// Prompt template for comparing two specs. Must contain {{old_spec}} and {{new_spec}}.
    #[arg(long, default_value = "prompts/update_spec.txt")]
    update_spec_prompt: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    info!("Scanning project at {:?}", args.root);
    let modules = collector::collect_modules(&args.root)?;
    info!("Found {} modules", modules.len());

    info!("Building dependency graph");
    let (graph, node_map) = graph::build_dependency_graph(&modules)?;

    // --- economy state (optional) ---
    let economy_state: Option<Arc<EconomyState>> = if args.incremental_economy {
        let update_prompt =
            std::fs::read_to_string(&args.update_spec_prompt).with_context(|| {
                format!("Failed to read update prompt {:?}", args.update_spec_prompt)
            })?;
        let changed = compute_changed_set(&modules)?;
        info!(
            "Economy: {} changed modules detected, deleting their specs",
            changed.len()
        );
        delete_changed_specs(&modules, &changed)?;
        Some(Arc::new(EconomyState::new(update_prompt, changed)))
    } else {
        None
    };

    if let Some(ec) = economy_state.as_deref() {
        propagate_affected_set(&modules, ec);
    }

    // Legacy --incremental still works (mutually exclusive in practice).
    if args.incremental && !args.incremental_economy {
        incremental_update(&modules, &graph, &node_map)?;
    }

    info!("Loading templates");
    let system_prompt = std::fs::read_to_string(&args.system_prompt)?;
    let final_spec_prompt = std::fs::read_to_string(&args.final_spec_prompt)?;
    let prelim_spec_prompt = std::fs::read_to_string(&args.prelim_spec_prompt)?;
    let partial_prompt = std::fs::read_to_string(&args.partial_prompt)?;
    let merge_prompt = std::fs::read_to_string(&args.merge_prompt)?;
    let spec_template = std::fs::read_to_string(&args.spec_template)?;

    let llm_config = llm::Config {
        api_base: args.api_base,
        api_key: args.api_key,
        model: args.model,
        max_tokens: args.max_tokens,
        temperature: args.temperature,
    };

    let stats = Arc::new(LlmStats::default());
    info!("Starting spec generation");
    processor::process_modules(
        &graph,
        &modules,
        processor::Prompts {
            system: &system_prompt,
            final_spec: &final_spec_prompt,
            prelim_spec: &prelim_spec_prompt,
            partial: &partial_prompt,
            merge: &merge_prompt,
            spec_template: &spec_template,
        },
        &llm_config,
        stats.clone(),
        economy_state.as_deref(),
    )
    .await?;

    info!("\n=== LLM Cumulative Statistics ===");
    info!(
        "Prompt tokens:   {}",
        stats.prompt_tokens.load(Ordering::Relaxed)
    );
    info!(
        "Completion tokens: {}",
        stats.completion_tokens.load(Ordering::Relaxed)
    );
    info!(
        "Total tokens:    {}",
        stats.total_tokens.load(Ordering::Relaxed)
    );
    info!(
        "Prompt chars:    {}",
        stats.prompt_chars.load(Ordering::Relaxed)
    );
    info!(
        "Completion chars: {}",
        stats.completion_chars.load(Ordering::Relaxed)
    );

    info!("All specifications generated successfully");
    Ok(())
}
