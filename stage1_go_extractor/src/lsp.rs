use std::{ops::ControlFlow, path::PathBuf, process::Stdio};

use anyhow::{Context, Result};
use async_lsp::{
    LanguageClient, LanguageServer, ResponseError, ServerSocket, concurrency::ConcurrencyLayer,
    panic::CatchUnwindLayer, router::Router, tracing::TracingLayer,
};
use lsp_types::{
    ClientCapabilities, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentSymbolClientCapabilities, DocumentSymbolParams, DocumentSymbolResponse,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams, InitializeParams,
    InitializedParams, LogMessageParams, Position, ProgressParams, PublishDiagnosticsParams, Range,
    SemanticTokenType, SemanticTokensClientCapabilities, SemanticTokensRangeParams,
    SemanticTokensRangeResult, ShowMessageParams, TextDocumentClientCapabilities,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, Url,
    WindowClientCapabilities, WorkspaceFolder,
};
use tokio::task::JoinHandle;
use tower::ServiceBuilder;

pub struct ClientArgs {
    pub language: String,
    pub lsp_bin: String,
    pub lsp_options: serde_json::Value,
    pub root_dir: PathBuf,
}

pub struct Client {
    server: ServerSocket,
    language: String,
    child: Option<async_process::Child>,
    task: Option<JoinHandle<Result<()>>>,
}

impl Client {
    pub async fn new(args: ClientArgs) -> Result<Self> {
        let state = State {};

        let mut router = Router::from_language_client(state);
        router.event(|_, _: StopEvent| ControlFlow::Break(Ok(())));

        let (mainloop, mut server) = async_lsp::MainLoop::new_client(|_server| {
            ServiceBuilder::new()
                .layer(TracingLayer::default())
                .layer(CatchUnwindLayer::default())
                .layer(ConcurrencyLayer::default())
                .service(router)
        });

        let mut child = async_process::Command::new(&args.lsp_bin)
            .current_dir(&args.root_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("failed to start LSP server")?;

        let stdout = child.stdout.take().unwrap();
        let stdin = child.stdin.take().unwrap();

        let task = tokio::spawn(async move {
            mainloop.run_buffered(stdout, stdin).await?;
            Ok(())
        });

        server
            .initialize(InitializeParams {
                workspace_folders: Some(vec![WorkspaceFolder {
                    uri: Url::from_file_path(&args.root_dir).unwrap(),
                    name: "root".into(),
                }]),
                capabilities: ClientCapabilities {
                    window: Some(WindowClientCapabilities {
                        work_done_progress: Some(true),
                        ..Default::default()
                    }),
                    text_document: Some(TextDocumentClientCapabilities {
                        document_symbol: Some(DocumentSymbolClientCapabilities {
                            hierarchical_document_symbol_support: Some(true),
                            ..Default::default()
                        }),
                        semantic_tokens: Some(SemanticTokensClientCapabilities {
                            token_types: vec![
                                SemanticTokenType::FUNCTION,
                                SemanticTokenType::METHOD,
                                SemanticTokenType::STRUCT,
                                SemanticTokenType::CLASS,
                                SemanticTokenType::VARIABLE,
                            ],
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                initialization_options: Some(args.lsp_options),
                ..Default::default()
            })
            .await?;

        server.initialized(InitializedParams {}).unwrap();

        Ok(Self {
            server,
            language: args.language,
            child: Some(child),
            task: Some(task),
        })
    }

    pub async fn open_document(&mut self, url: &Url, text: String) -> Result<()> {
        self.server.did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: url.clone(),
                language_id: self.language.clone(),
                version: 0,
                text,
            },
        })?;
        Ok(())
    }

    pub async fn close_document(&mut self, url: &Url) -> Result<()> {
        self.server.did_close(DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier::new(url.clone()),
        })?;
        Ok(())
    }

    pub async fn document_symbols(&mut self, url: &Url) -> Result<Option<DocumentSymbolResponse>> {
        Ok(self
            .server
            .document_symbol(DocumentSymbolParams {
                text_document: TextDocumentIdentifier::new(url.clone()),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await?)
    }

    pub async fn semantic_tokens(
        &mut self,
        url: &Url,
        range: Range,
    ) -> Result<Option<SemanticTokensRangeResult>> {
        Ok(self
            .server
            .semantic_tokens_range(SemanticTokensRangeParams {
                text_document: TextDocumentIdentifier::new(url.clone()),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                range,
            })
            .await?)
    }

    pub async fn hover(&mut self, url: &Url, position: Position) -> Result<Option<Hover>> {
        Ok(self
            .server
            .hover(HoverParams {
                text_document_position_params: TextDocumentPositionParams::new(
                    TextDocumentIdentifier::new(url.clone()),
                    position,
                ),
                work_done_progress_params: Default::default(),
            })
            .await?)
    }

    pub async fn definition(
        &mut self,
        url: &Url,
        position: Position,
    ) -> Result<Option<GotoDefinitionResponse>> {
        Ok(self
            .server
            .definition(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams::new(
                    TextDocumentIdentifier::new(url.clone()),
                    position,
                ),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .await?)
    }

    pub async fn stop(mut self) -> Result<()> {
        self.server.shutdown(()).await?;
        self.server.exit(())?;
        self.server.emit(StopEvent)?;
        self.task.take().unwrap().await??;
        self.child.take();
        Ok(())
    }
}

struct StopEvent;

struct State {}

impl LanguageClient for State {
    type Error = ResponseError;
    type NotifyResult = ControlFlow<async_lsp::Result<()>>;

    fn progress(&mut self, params: ProgressParams) -> Self::NotifyResult {
        tracing::debug!("{:?} {:?}", params.token, params.value);
        ControlFlow::Continue(())
    }

    fn publish_diagnostics(&mut self, _: PublishDiagnosticsParams) -> Self::NotifyResult {
        ControlFlow::Continue(())
    }

    fn show_message(&mut self, params: ShowMessageParams) -> Self::NotifyResult {
        tracing::debug!("Message {:?}: {}", params.typ, params.message);
        ControlFlow::Continue(())
    }

    fn log_message(&mut self, params: LogMessageParams) -> Self::NotifyResult {
        tracing::debug!("Message {:?}: {}", params.typ, params.message);
        ControlFlow::Continue(())
    }
}
