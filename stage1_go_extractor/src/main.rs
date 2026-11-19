mod lsp;
mod schema;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use anyhow::{Result, anyhow};
use clap::Parser;
use ignore::WalkBuilder;
use lsp_types::{
    DocumentSymbol, DocumentSymbolResponse, GotoDefinitionResponse, HoverContents, Position, Range,
    SemanticTokensRangeResult, SymbolKind, Url,
};
use regex::Regex;

#[derive(Parser)]
struct Args {
    /// Path to `go.mod` file
    manifest: PathBuf,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt::init();

    let manifest_path = args.manifest.canonicalize()?;
    let root_dir = manifest_path
        .parent()
        .ok_or_else(|| anyhow!("Invalid manifest path"))?
        .canonicalize()?;

    let mut lsp = lsp::Client::new(lsp::ClientArgs {
        language: "go".to_string(),
        lsp_bin: "gopls".into(),
        lsp_options: serde_json::json!({
            "linksInHover": false,
            "semanticTokens": true
        }),
        root_dir: root_dir.clone(),
    })
    .await?;

    let walk = WalkBuilder::new(&root_dir)
        .standard_filters(true)
        .types(
            ignore::types::TypesBuilder::new()
                .add_defaults()
                .select("go")
                .build()
                .unwrap(),
        )
        .build();

    for entry in walk {
        let entry = entry?;
        if !entry.file_type().is_some_and(|s| s.is_file()) {
            continue;
        }

        let url = Url::from_file_path(entry.path()).unwrap();
        let file_path = entry.path().to_owned();
        let rel_path = entry.path().strip_prefix(&root_dir)?;
        let package_path = rel_path.parent().unwrap().to_string_lossy().to_string();

        tracing::info!("Processing {}", rel_path.display());

        let text = tokio::fs::read_to_string(url.path()).await?;
        let num_lines = text.lines().count();

        lsp.open_document(&url, text).await?;

        let symbols = extract_public_symbols(&mut lsp, &url).await?;
        let dependencies = extract_deps(&mut lsp, &root_dir, &url, num_lines).await?;

        lsp.close_document(&url).await?;

        let facts = schema::ModuleFacts {
            package_path: package_path.clone(),
            file_path: file_path.clone(),
            public_symbols: symbols,
        };

        let deps = schema::ModuleDeps {
            package_path,
            file_path,
            dependencies,
        };

        let facts_json = serde_json::to_string_pretty(&facts)?;
        let facts_path = entry.path().with_added_extension("facts.json");
        tokio::fs::write(facts_path, facts_json).await?;

        let deps_json = serde_json::to_string_pretty(&deps)?;
        let deps_path = entry.path().with_added_extension("deps.json");
        tokio::fs::write(deps_path, deps_json).await?;
    }

    lsp.stop().await?;

    Ok(())
}

async fn extract_public_symbols(lsp: &mut lsp::Client, url: &Url) -> Result<schema::Symbols> {
    let mut res = schema::Symbols::default();

    let Some(DocumentSymbolResponse::Nested(symbols)) = lsp.document_symbols(url).await? else {
        tracing::warn!("Missing symbols");
        return Ok(res);
    };

    for symbol in symbols {
        if !is_symbol_public(&symbol) {
            continue;
        }

        match symbol.kind {
            SymbolKind::FUNCTION => {
                res.functions
                    .push(extract_function(lsp, url, symbol).await?);
            }
            SymbolKind::METHOD => {
                res.methods.push(extract_method(lsp, url, symbol).await?);
            }
            SymbolKind::STRUCT => {
                res.structs.push(extract_struct(lsp, url, symbol).await?);
            }
            SymbolKind::CLASS => {
                res.types.push(extract_type(lsp, url, symbol).await?);
            }
            _ => {}
        }
    }

    Ok(res)
}

async fn get_hover_markdown(
    lsp: &mut lsp::Client,
    url: &Url,
    symbol: &DocumentSymbol,
) -> Result<String> {
    let hover = lsp.hover(url, symbol.selection_range.start).await?;

    if let Some(hover) = hover
        && let HoverContents::Markup(markup) = hover.contents
    {
        Ok(markup.value)
    } else {
        Ok("".to_string())
    }
}

fn extract_markdown_doc(markdown: String) -> String {
    static RE_IGNORE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?imsx)
            (?:
              (?:
                ^```go \s*
                  (?:.*?)
                ^``` \s* $
              )
              | ^ \s* --- \s* $
              | ^ \s* Embedded \s+ fields: \s* $
              | ^ \s* Promoted \s+ methods: \s* $
            )
        "#,
        )
        .unwrap()
    });

    RE_IGNORE.replace_all(&markdown, "").trim().to_string()
}

async fn get_hover_doc(
    lsp: &mut lsp::Client,
    url: &Url,
    symbol: &DocumentSymbol,
) -> Result<String> {
    get_hover_markdown(lsp, url, symbol)
        .await
        .map(extract_markdown_doc)
}

fn is_symbol_public(symbol: &DocumentSymbol) -> bool {
    let name = &symbol.name;

    match symbol.kind {
        SymbolKind::METHOD => {
            // for methods, the format is (...).MethodName so we look for the dot
            name.rfind('.')
                .map(|idx| starts_uppercase(&name[idx + 1..]))
                .unwrap_or(false)
        }
        _ => starts_uppercase(&symbol.name),
    }
}

fn starts_uppercase(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_uppercase())
}

async fn extract_function(
    lsp: &mut lsp::Client,
    url: &Url,
    symbol: DocumentSymbol,
) -> Result<schema::FunctionInfo> {
    let doc = get_hover_doc(lsp, url, &symbol).await?;
    Ok(schema::FunctionInfo {
        name: symbol.name,
        signature: symbol.detail.unwrap_or_default(),
        doc,
    })
}

async fn extract_method(
    lsp: &mut lsp::Client,
    url: &Url,
    symbol: DocumentSymbol,
) -> Result<schema::MethodInfo> {
    let doc = get_hover_doc(lsp, url, &symbol).await?;
    Ok(schema::MethodInfo {
        name: symbol.name,
        signature: symbol.detail.unwrap_or_default(),
        doc,
    })
}

async fn extract_struct(
    lsp: &mut lsp::Client,
    url: &Url,
    symbol: DocumentSymbol,
) -> Result<schema::StructInfo> {
    let doc = get_hover_doc(lsp, url, &symbol).await?;

    let mut fields = Vec::new();

    for child in symbol.children.into_iter().flatten() {
        if child.kind != SymbolKind::FIELD || !is_symbol_public(&child) {
            continue;
        }
        fields.push(extract_field(lsp, url, child).await?);
    }

    Ok(schema::StructInfo {
        name: symbol.name,
        doc,
        fields,
    })
}

async fn extract_field(
    lsp: &mut lsp::Client,
    url: &Url,
    symbol: DocumentSymbol,
) -> Result<schema::FieldInfo> {
    let doc = get_hover_doc(lsp, url, &symbol).await?;
    Ok(schema::FieldInfo {
        name: symbol.name,
        r#type: symbol.detail.unwrap_or_default(),
        doc,
    })
}

async fn extract_type(
    lsp: &mut lsp::Client,
    url: &Url,
    symbol: DocumentSymbol,
) -> Result<schema::TypeInfo> {
    let doc = get_hover_doc(lsp, url, &symbol).await?;
    Ok(schema::TypeInfo {
        name: symbol.name,
        signature: symbol.detail.unwrap_or_default(),
        doc,
    })
}

async fn extract_deps(
    lsp: &mut lsp::Client,
    root_dir: &Path,
    url: &Url,
    num_lines: usize,
) -> Result<HashSet<schema::Dependency>> {
    let mut deps = HashSet::new();

    let self_path = url.to_file_path().unwrap();

    let mut range = Range::new(Position::new(0, 0), Position::new(100, 0));

    loop {
        range.end.line = range.end.line.min(num_lines as u32);

        let tokens = match lsp.semantic_tokens(url, range).await {
            Ok(Some(SemanticTokensRangeResult::Tokens(v))) => v.data,
            Ok(Some(SemanticTokensRangeResult::Partial(v))) => v.data,
            _ => return Ok(deps),
        };

        range.start = range.end;
        range.end.line += 100;

        if tokens.is_empty() {
            break;
        }

        let mut pos = Position::new(0, 0);
        for token in tokens {
            if token.delta_line > 0 {
                pos.line += token.delta_line;
                pos.character = token.delta_start;
            } else {
                pos.character += token.delta_start;
            }

            let Ok(Some(def)) = lsp.definition(url, pos).await else {
                continue;
            };

            let urls = match def {
                GotoDefinitionResponse::Scalar(v) => vec![v.uri],
                GotoDefinitionResponse::Array(v) => v.into_iter().map(|l| l.uri).collect(),
                GotoDefinitionResponse::Link(v) => v.into_iter().map(|l| l.target_uri).collect(),
            };

            for url in urls {
                let Ok(file_path) = url.to_file_path() else {
                    continue;
                };

                if file_path == self_path || !file_path.exists() {
                    continue;
                }

                let Ok(rel_path) = file_path.strip_prefix(root_dir) else {
                    continue;
                };

                let package_path = rel_path.parent().unwrap().to_string_lossy().to_string();

                deps.insert(schema::Dependency {
                    package_path,
                    spec_path: file_path.with_added_extension("spec.md"),
                    file_path,
                });
            }
        }
    }

    Ok(deps)
}
