mod collector;
mod graph;
mod hash;
mod incremental;
mod llm;
mod processor;
mod template;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::Result;
use clap::Parser;
use log::info;

use llm::LlmStats;

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

    /// API key. Prefer supplying this via the `LLM_API_KEY` environment
    /// variable so it does not leak into the process list / shell history.
    #[arg(long, env = "LLM_API_KEY", default_value = "")]
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

    #[arg(long)]
    incremental: bool,
}

fn load_prompts(args: &Args) -> Result<processor::Prompts> {
    let system = std::fs::read_to_string(&args.system_prompt)?;
    let final_spec = std::fs::read_to_string(&args.final_spec_prompt)?;
    let prelim_spec = std::fs::read_to_string(&args.prelim_spec_prompt)?;
    let partial = std::fs::read_to_string(&args.partial_prompt)?;
    let merge = std::fs::read_to_string(&args.merge_prompt)?;
    let spec_template = std::fs::read_to_string(&args.spec_template)?;

    Ok(processor::Prompts {
        system,
        final_spec,
        prelim_spec,
        partial,
        merge,
        spec_template,
    })
}

fn log_llm_stats(stats: &LlmStats) {
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
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    if args.api_base.trim().is_empty() {
        anyhow::bail!("--api-base must not be empty");
    }
    if args.model.trim().is_empty() {
        anyhow::bail!("--model must not be empty");
    }

    info!("Scanning project at {:?}", args.root);
    let modules = collector::collect_modules(&args.root)?;
    info!("Found {} modules", modules.len());

    info!("Building dependency graph");
    let (graph, node_map) = graph::build_dependency_graph(&modules)?;

    if args.incremental {
        incremental::incremental_update(&modules, &graph, &node_map)?;
    }

    let prompts = load_prompts(&args)?;

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
        Arc::new(graph),
        Arc::from(modules),
        prompts,
        &llm_config,
        stats.clone(),
        args.concurrency,
    )
    .await?;

    log_llm_stats(&stats);

    info!("All specifications generated successfully");
    Ok(())
}
