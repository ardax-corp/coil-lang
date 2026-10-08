//! Coil language server. The transport is standard LSP JSON-RPC over stdio.
#![allow(deprecated)]

use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    path::{Path, PathBuf},
};

use clap::Command as ClapCommand;
use clap::{CommandFactory, Parser};
use coil_args::{HostGrantFlags, RootFlags, parse_with, print_cli_error, print_command_help};
use compiler::{
    BuiltinExport, Checker, HostGrants, ProjectIndex, SymbolIndex, SymbolKind, VirtualModules,
    format_ty_for_diag,
};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response, ResponseError};
use lsp_types::{
    Command, CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, Diagnostic,
    DiagnosticRelatedInformation, DiagnosticSeverity, Documentation, DocumentFormattingParams,
    DocumentHighlight, DocumentHighlightParams, DocumentRangeFormattingParams, DocumentSymbol,
    DocumentSymbolParams, FoldingRange, FoldingRangeParams, GotoDefinitionParams, Hover,
    HoverContents, HoverParams, InitializeParams, InitializeResult, Location, MarkupContent,
    InsertTextFormat, MarkupKind, ParameterInformation, Position, PublishDiagnosticsParams,
    Range as LspRange,
    ReferenceParams, SelectionRange, SelectionRangeParams, SemanticToken, SemanticTokenType,
    SemanticTokens, SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions,
    SemanticTokensParams, SemanticTokensRangeParams, ServerCapabilities, SignatureHelp, SignatureInformation,
    SymbolInformation, TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Uri,
    WorkspaceSymbolParams,
};
use parser::{
    Pratt,
    ast::{Expression, Output},
    format_source,
};
use reporting::{Label, Message as CoilMessage, MessageKind};
use serde_json::Value;

#[derive(Default)]
struct Document {
    text: String,
    version: i32,
    /// Last successfully typechecked snapshot for completions/hover when the
    /// current buffer does not parse (e.g. partial identifier while typing).
    last_good: Option<GoodAnalysis>,
}

#[derive(Clone)]
struct GoodAnalysis {
    candidates: HashMap<String, CompletionCandidate>,
}

#[derive(Default)]
struct ServerState {
    documents: HashMap<Uri, Document>,
    project_index: Option<ProjectIndex>,
    workspace_root: Option<PathBuf>,
    last_typecheck: Vec<(PathBuf, Vec<CoilMessage>)>,
    /// URIs (as strings: `Uri` is not a sound hash key) whose last
    /// published diagnostics were non-empty.
    dirty_uris: HashSet<String>,
    /// Entry of `last_typecheck`; the project checker holds its span types.
    last_entry: Option<PathBuf>,
    /// Edited documents whose re-analysis waits until the queued edits
    /// behind them are applied (one typecheck per burst of keystrokes).
    pending_analysis: Vec<Uri>,
    /// `--root` dirs from the command line, searched after the defaults.
    extra_roots: Vec<PathBuf>,
}

/// Command-line options: extra module roots and host grants, spelled as for
/// `coil compile` so tools can pass the same flags to every subcommand.
#[derive(Debug, Default, PartialEq)]
struct LspOptions {
    /// Absolute (relative ones resolve against the current directory).
    extra_roots: Vec<PathBuf>,
    grants: HostGrants,
}

#[derive(Parser, Debug)]
#[command(
    name = "coil-lsp",
    about = "Start the Coil language server over stdin/stdout",
    disable_help_subcommand = true,
    after_help = "`--root` is searched after `src`, `.` and `.deps/*/src`.\n\
`--stdio` is accepted for LSP clients; stdio is the only transport."
)]
struct LspCli {
    #[command(flatten)]
    grants: HostGrantFlags,
    #[command(flatten)]
    roots: RootFlags,
    /// Accepted for LSP clients; stdio is the only transport
    #[arg(long)]
    stdio: bool,
}

fn lsp_command() -> ClapCommand {
    let mut command = LspCli::command();
    command.set_bin_name("coil lsp");
    command
}

/// `Ok(None)` when help was asked for.
fn parse_options(
    args: impl IntoIterator<Item = String>,
    cwd: &Path,
) -> Result<Option<LspOptions>, String> {
    let mut argv = vec!["coil-lsp".to_string()];
    argv.extend(args);
    let Some(cli) = parse_with::<LspCli>(lsp_command(), &argv)? else {
        return Ok(None);
    };
    let _ = cli.stdio;
    let mut grants = cli.grants.into_grants();
    grants.ffi_search_paths = grants
        .ffi_search_paths
        .into_iter()
        .map(|path| cwd.join(path))
        .collect();
    Ok(Some(LspOptions {
        extra_roots: cli.roots.root.into_iter().map(|path| cwd.join(path)).collect(),
        grants,
    }))
}

fn main() {
    comptime::install();
    let cwd = std::env::current_dir().unwrap_or_default();
    let options = match parse_options(std::env::args().skip(1), &cwd) {
        Ok(Some(options)) => options,
        Ok(None) => {
            print_command_help(lsp_command(), "coil lsp");
            return;
        }
        Err(error) => {
            print_cli_error(&error);
            std::process::exit(2);
        }
    };
    let (connection, io_threads) = Connection::stdio();
    let result = run(&connection, options);
    let code = match &result {
        Ok(()) => 0,
        // stdin closed (even mid-handshake): a normal shutdown.
        Err(error) if is_disconnect(error.as_ref()) => 0,
        Err(error) => {
            eprintln!("coil-lsp: {error}");
            1
        }
    };
    finish_io(connection, io_threads);
    std::process::exit(code);
}

/// Whether `error` is lsp-server's "the client went away" (stdin EOF).
fn is_disconnect(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<lsp_server::ProtocolError>()
        .is_some_and(|e| e.channel_is_disconnected())
}

/// Flush queued messages before exiting (#583): dropping the connection
/// lets the writer thread drain its channel, and joining waits for it.
/// `exit` while the writer still held a response lost it, e.g. the
/// `initialize` result when stdin closed right after the request.
/// `IoThreads::join` also waits for the reader, which only returns at
/// stdin EOF, so a client that keeps stdin open after an error gets a
/// bounded wait instead of a hang.
fn finish_io(connection: Connection, io_threads: lsp_server::IoThreads) {
    drop(connection);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = io_threads.join();
        let _ = done_tx.send(());
    });
    let _ = done_rx.recv_timeout(std::time::Duration::from_secs(2));
}

fn run(connection: &Connection, options: LspOptions) -> Result<(), Box<dyn std::error::Error>> {
    let (request_id, initialize_params) = connection.initialize_start()?;
    let _params: InitializeParams = serde_json::from_value(initialize_params)?;
    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::INCREMENTAL)),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        document_symbol_provider: Some(lsp_types::OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".into(), ":".into()]),
            ..CompletionOptions::default()
        }),
        signature_help_provider: Some(lsp_types::SignatureHelpOptions {
            trigger_characters: Some(vec!["(".into(), ",".into()]),
            retrigger_characters: Some(vec![",".into()]),
            ..lsp_types::SignatureHelpOptions::default()
        }),
        document_formatting_provider: Some(lsp_types::OneOf::Left(true)),
        document_range_formatting_provider: Some(lsp_types::OneOf::Left(true)),
        document_highlight_provider: Some(lsp_types::OneOf::Left(true)),
        references_provider: Some(lsp_types::OneOf::Left(true)),
        definition_provider: Some(lsp_types::OneOf::Left(true)),
        type_definition_provider: Some(
            lsp_types::TypeDefinitionProviderCapability::Simple(true),
        ),
        folding_range_provider: Some(lsp_types::FoldingRangeProviderCapability::Simple(true)),
        selection_range_provider: Some(lsp_types::SelectionRangeProviderCapability::Simple(true)),
        rename_provider: Some(lsp_types::OneOf::Right(lsp_types::RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        inlay_hint_provider: Some(lsp_types::OneOf::Left(true)),
        code_action_provider: Some(lsp_types::CodeActionProviderCapability::Options(
            lsp_types::CodeActionOptions {
                code_action_kinds: Some(vec![
                    lsp_types::CodeActionKind::QUICKFIX,
                    lsp_types::CodeActionKind::REFACTOR_REWRITE,
                ]),
                ..lsp_types::CodeActionOptions::default()
            },
        )),
        semantic_tokens_provider: Some(
            SemanticTokensOptions {
                legend: SemanticTokensLegend {
                    token_types: vec![
                        SemanticTokenType::KEYWORD,
                        SemanticTokenType::FUNCTION,
                        SemanticTokenType::TYPE,
                        SemanticTokenType::VARIABLE,
                        SemanticTokenType::COMMENT,
                        SemanticTokenType::STRING,
                        SemanticTokenType::NUMBER,
                        SemanticTokenType::NAMESPACE,
                        SemanticTokenType::OPERATOR,
                        SemanticTokenType::MACRO,
                    ],
                    token_modifiers: Vec::new(),
                },
                range: Some(true),
                full: Some(SemanticTokensFullOptions::Bool(true)),
                ..SemanticTokensOptions::default()
            }
            .into(),
        ),
        ..ServerCapabilities::default()
    };
    let result = InitializeResult {
        capabilities,
        server_info: Some(lsp_types::ServerInfo {
            name: "coil-lsp".into(),
            version: Some(env!("CARGO_PKG_VERSION").into()),
        }),
    };
    connection.initialize_finish(request_id, serde_json::to_value(result)?)?;

    let mut state = ServerState {
        extra_roots: options.extra_roots,
        ..ServerState::default()
    };
    let workspace_root = _params
        .root_uri
        .as_ref()
        .and_then(uri_path)
        .or_else(|| {
            _params
                .workspace_folders
                .as_ref()
                .and_then(|folders| folders.first())
                .and_then(|folder| uri_path(&folder.uri))
        });
    if let Some(root_uri) = workspace_root {
        state.workspace_root = Some(root_uri.clone());
        let mut index = ProjectIndex::with_roots(
            root_uri.clone(),
            lsp_module_roots(&root_uri, &state.extra_roots),
        );
        // Without the project's grants, `env::exec` & co. are false errors.
        index.pipeline_mut().set_host_grants(options.grants);
        state.project_index = Some(index);
    }
    // Messages read ahead of the one being handled: cancellations and
    // edits behind a slow request apply before it runs.
    let mut queue: VecDeque<Message> = VecDeque::new();
    let mut cancelled: HashSet<RequestId> = HashSet::new();
    loop {
        if queue.is_empty() {
            flush_pending_analysis(connection, &mut state)?;
            match connection.receiver.recv() {
                Ok(message) => queue.push_back(message),
                Err(_) => break,
            }
        }
        while let Ok(message) = connection.receiver.try_recv() {
            queue.push_back(message);
        }
        cancelled.extend(take_cancellations(&mut queue));
        let Some(message) = queue.pop_front() else {
            continue;
        };
        match message {
            Message::Request(request) => {
                if cancelled.remove(&request.id) {
                    send_error(
                        connection,
                        request.id,
                        ErrorCode::RequestCanceled,
                        format!("`{}` was cancelled", request.method),
                    )?;
                    continue;
                }
                if request.method == "shutdown" {
                    send_response(connection, request.id, Value::Null)?;
                    continue;
                }
                flush_pending_analysis(connection, &mut state)?;
                match handle_request(&mut state, &request) {
                    Ok(Some(value)) => send_response(connection, request.id, value)?,
                    // Every request needs a reply; silence hangs the client.
                    Ok(None) => send_error(
                        connection,
                        request.id,
                        ErrorCode::MethodNotFound,
                        format!("unsupported request `{}`", request.method),
                    )?,
                    Err(error) => send_error(
                        connection,
                        request.id,
                        ErrorCode::InvalidParams,
                        format!("{}: {error}", request.method),
                    )?,
                }
            }
            Message::Notification(notification) => {
                if notification.method == "exit" {
                    break;
                }
                if notification.method != "textDocument/didChange" {
                    flush_pending_analysis(connection, &mut state)?;
                }
                // A bad notification must not take the server down.
                if let Err(error) = handle_notification(connection, &mut state, &notification) {
                    eprintln!("coil-lsp: {}: {error}", notification.method);
                }
            }
            Message::Response(_) => {}
        }
    }
    Ok(())
}

fn send_response(
    connection: &Connection,
    id: RequestId,
    result: Value,
) -> Result<(), Box<dyn std::error::Error>> {
    connection.sender.send(Message::Response(Response {
        id,
        result: Some(result),
        error: None,
    }))?;
    Ok(())
}

fn send_error(
    connection: &Connection,
    id: RequestId,
    code: ErrorCode,
    message: String,
) -> Result<(), Box<dyn std::error::Error>> {
    connection.sender.send(Message::Response(Response {
        id,
        result: None,
        error: Some(ResponseError {
            code: code as i32,
            message,
            data: None,
        }),
    }))?;
    Ok(())
}

fn handle_request(
    state: &mut ServerState,
    request: &Request,
) -> Result<Option<Value>, Box<dyn std::error::Error>> {
    let value = match request.method.as_str() {
        "textDocument/formatting" => {
            let params: DocumentFormattingParams = serde_json::from_value(request.params.clone())?;
            let Some(document) = state.documents.get(&params.text_document.uri) else {
                return Ok(Some(serde_json::to_value(Vec::<TextEdit>::new())?));
            };
            let edits = match format_source(&document.text) {
                Ok(formatted) if formatted != document.text => vec![TextEdit {
                    range: full_range(&document.text),
                    new_text: formatted,
                }],
                _ => Vec::new(),
            };
            Some(serde_json::to_value(edits)?)
        }
        "textDocument/rangeFormatting" => {
            let params: DocumentRangeFormattingParams =
                serde_json::from_value(request.params.clone())?;
            let Some(document) = state.documents.get(&params.text_document.uri) else {
                return Ok(Some(serde_json::to_value(Vec::<TextEdit>::new())?));
            };
            let Some(byte_span) = lsp_range_to_byte_range(&document.text, params.range) else {
                return Ok(Some(serde_json::to_value(Vec::<TextEdit>::new())?));
            };
            let edits = format_requested_range(&document.text, byte_span).unwrap_or_default();
            Some(serde_json::to_value(edits)?)
        }
        "textDocument/documentSymbol" => {
            let params: DocumentSymbolParams = serde_json::from_value(request.params.clone())?;
            let symbols = state
                .documents
                .get(&params.text_document.uri)
                .map(|document| document_symbols(&document.text))
                .unwrap_or_default();
            Some(serde_json::to_value(symbols)?)
        }
        "workspace/symbol" => {
            let params: WorkspaceSymbolParams = serde_json::from_value(request.params.clone())?;
            Some(serde_json::to_value(workspace_symbols(state, &params.query))?)
        }
        "textDocument/foldingRange" => {
            let params: FoldingRangeParams = serde_json::from_value(request.params.clone())?;
            let ranges = state
                .documents
                .get(&params.text_document.uri)
                .map(|document| {
                    document_symbols(&document.text)
                        .into_iter()
                        .filter_map(|symbol| {
                            (symbol.range.start.line < symbol.range.end.line).then_some(
                                FoldingRange {
                                    start_line: symbol.range.start.line,
                                    start_character: Some(symbol.range.start.character),
                                    end_line: symbol.range.end.line,
                                    end_character: Some(symbol.range.end.character),
                                    kind: None,
                                    collapsed_text: None,
                                },
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Some(serde_json::to_value(ranges)?)
        }
        "textDocument/selectionRange" => {
            let params: SelectionRangeParams = serde_json::from_value(request.params.clone())?;
            let ranges = state
                .documents
                .get(&params.text_document.uri)
                .map(|document| selection_ranges(&document.text, &params.positions))
                .unwrap_or_default();
            Some(serde_json::to_value(ranges)?)
        }
        "textDocument/codeAction" => {
            let params: lsp_types::CodeActionParams = serde_json::from_value(request.params.clone())?;
            let actions = code_actions(
                state,
                &params.text_document.uri,
                params.range,
                &params.context.diagnostics,
            )
            .unwrap_or_default();
            Some(serde_json::to_value(actions)?)
        }
        "textDocument/inlayHint" => {
            let params: lsp_types::InlayHintParams = serde_json::from_value(request.params.clone())?;
            let hints = inlay_hints(state, &params.text_document.uri, params.range).unwrap_or_default();
            Some(serde_json::to_value(hints)?)
        }
        "textDocument/hover" => {
            let params: HoverParams = serde_json::from_value(request.params.clone())?;
            let hover = project_hover(
                state,
                &params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            )
            .or_else(|| {
                state
                    .documents
                    .get(&params.text_document_position_params.text_document.uri)
                    .and_then(|document| {
                        hover(document, params.text_document_position_params.position)
                    })
            });
            Some(serde_json::to_value(hover)?)
        }
        "textDocument/completion" => {
            let params: CompletionParams = serde_json::from_value(request.params.clone())?;
            let uri = &params.text_document_position.text_document.uri;
            let position = params.text_document_position.position;
            // `recv.` / `recv.pre`: the receiver's fields and methods.
            let items = match member_completions(state, uri, position) {
                Some(items) => items,
                None => state
                    .documents
                    .get(uri)
                    .map(|document| completions(document, position))
                    .unwrap_or_default(),
            };
            Some(serde_json::to_value(items)?)
        }
        "textDocument/signatureHelp" => {
            let params: lsp_types::SignatureHelpParams =
                serde_json::from_value(request.params.clone())?;
            let uri = &params.text_document_position_params.text_document.uri;
            let position = params.text_document_position_params.position;
            let signature = decl_signature_help(state, uri, position).or_else(|| {
                state
                    .documents
                    .get(uri)
                    .and_then(|document| signature_help(document, position))
            });
            Some(serde_json::to_value(signature)?)
        }
        "textDocument/documentHighlight" => {
            let params: DocumentHighlightParams = serde_json::from_value(request.params.clone())?;
            let highlights = identifier_highlights(
                state,
                &params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            );
            Some(serde_json::to_value(highlights)?)
        }
        "textDocument/references" => {
            let params: ReferenceParams = serde_json::from_value(request.params.clone())?;
            let locations = identifier_locations(
                state,
                &params.text_document_position.text_document.uri,
                params.text_document_position.position,
                params.context.include_declaration,
            );
            Some(serde_json::to_value(locations)?)
        }
        "textDocument/definition" | "textDocument/typeDefinition" => {
            let params: GotoDefinitionParams = serde_json::from_value(request.params.clone())?;
            if let Some(location) = member_definition(
                state,
                &params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            ) {
                return Ok(Some(serde_json::to_value(vec![location])?));
            }
            let locations = goto_definitions(
                state,
                &params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            );
            Some(serde_json::to_value(locations)?)
        }
        "textDocument/semanticTokens/full" => {
            let params: SemanticTokensParams = serde_json::from_value(request.params.clone())?;
            let tokens = state
                .documents
                .get(&params.text_document.uri)
                .map(|document| {
                    semantic_tokens(&document.text, uri_path(&params.text_document.uri), None)
                })
                .unwrap_or_default();
            Some(serde_json::to_value(Some(SemanticTokens {
                result_id: None,
                data: tokens,
            }))?)
        }
        "textDocument/semanticTokens/range" => {
            let params: SemanticTokensRangeParams =
                serde_json::from_value(request.params.clone())?;
            let tokens = state
                .documents
                .get(&params.text_document.uri)
                .and_then(|document| {
                    let byte_range = lsp_range_to_byte_range(&document.text, params.range)?;
                    Some(semantic_tokens(
                        &document.text,
                        uri_path(&params.text_document.uri),
                        Some(byte_range),
                    ))
                })
                .unwrap_or_default();
            Some(serde_json::to_value(Some(SemanticTokens {
                result_id: None,
                data: tokens,
            }))?)
        }
        "textDocument/prepareRename" => {
            let params: lsp_types::TextDocumentPositionParams =
                serde_json::from_value(request.params.clone())?;
            let range = state.documents.get(&params.text_document.uri).and_then(|document| {
                let offset = position_to_byte(&document.text, params.position)?;
                let word = word_range(&document.text, offset)?;
                let name = &document.text[word.clone()];
                if coil_keywords().contains(&name) || name == "self" {
                    return None;
                }
                // Only symbols we can resolve: renaming a stray word would
                // silently leave its real uses behind.
                let found = !identifier_locations(
                    state,
                    &params.text_document.uri,
                    params.position,
                    true,
                )
                .is_empty();
                found.then(|| byte_range(&document.text, &word))
            });
            Some(serde_json::to_value(range)?)
        }
        "textDocument/rename" => {
            let params: lsp_types::RenameParams = serde_json::from_value(request.params.clone())?;
            if !is_valid_identifier(&params.new_name) {
                return Err(format!("`{}` is not a valid identifier", params.new_name).into());
            }
            let edits = rename_identifier(
                state,
                &params.text_document_position.text_document.uri,
                params.text_document_position.position,
                &params.new_name,
            );
            Some(serde_json::to_value(edits)?)
        }
        _ => None,
    };
    Ok(value)
}

fn handle_notification(
    connection: &Connection,
    state: &mut ServerState,
    notification: &Notification,
) -> Result<(), Box<dyn std::error::Error>> {
    match notification.method.as_str() {
        "textDocument/didOpen" => {
            let params: lsp_types::DidOpenTextDocumentParams =
                serde_json::from_value(notification.params.clone())?;
            let text = params.text_document.text.clone();
            let last_good = analyze_for_completions(&text).or_else(|| {
                // Mid-edit buffers (e.g. trailing incomplete ident) still seed
                // completions/hover metadata when a local sanitize parses.
                let offset = text.len().saturating_sub(1);
                analyze_for_completions_at(&text, Some(offset))
            });
            state.documents.insert(
                params.text_document.uri.clone(),
                Document {
                    text,
                    version: params.text_document.version,
                    last_good,
                },
            );
            if let Some(path) = uri_path(&params.text_document.uri) {
                refresh_project(state, &path);
            }
            publish_diagnostics(connection, state, &params.text_document.uri)?;
        }
        "textDocument/didChange" => {
            let params: lsp_types::DidChangeTextDocumentParams =
                serde_json::from_value(notification.params.clone())?;
            let uri = params.text_document.uri;
            if let Some(document) = state.documents.get_mut(&uri) {
                for change in params.content_changes {
                    apply_change(&mut document.text, change);
                }
                document.version = params.text_document.version;
                if !state.pending_analysis.contains(&uri) {
                    state.pending_analysis.push(uri);
                }
            }
        }
        "textDocument/didSave" => {
            let params: lsp_types::DidSaveTextDocumentParams =
                serde_json::from_value(notification.params.clone())?;
            publish_diagnostics(connection, state, &params.text_document.uri)?;
        }
        "textDocument/didClose" => {
            let params: lsp_types::DidCloseTextDocumentParams =
                serde_json::from_value(notification.params.clone())?;
            if let Some(path) = uri_path(&params.text_document.uri)
                && let Some(index) = state.project_index.as_mut() {
                    index.pipeline_mut().clear_file_text(&path);
                }
            state.documents.remove(&params.text_document.uri);
            let params = PublishDiagnosticsParams {
                uri: params.text_document.uri,
                diagnostics: Vec::new(),
                version: None,
            };
            connection
                .sender
                .send(Message::Notification(Notification::new(
                    "textDocument/publishDiagnostics".into(),
                    params,
                )))?;
        }
        _ => {}
    }
    Ok(())
}

/// Apply one `didChange` content change: a ranged splice, or the whole text.
fn apply_change(text: &mut String, change: lsp_types::TextDocumentContentChangeEvent) {
    match change.range.and_then(|range| lsp_range_to_byte_range(text, range)) {
        Some(range) => text.replace_range(range, &change.text),
        None => *text = change.text,
    }
}

/// Remove `$/cancelRequest` notifications from `queue`; return the ids of
/// the queued requests they cancel (cancels for requests already answered
/// are dropped).
fn take_cancellations(queue: &mut VecDeque<Message>) -> Vec<RequestId> {
    let mut ids = Vec::new();
    queue.retain(|message| {
        let Message::Notification(notification) = message else {
            return true;
        };
        if notification.method != "$/cancelRequest" {
            return true;
        }
        if let Ok(params) = serde_json::from_value::<lsp_types::CancelParams>(notification.params.clone()) {
            ids.push(match params.id {
                lsp_types::NumberOrString::Number(n) => RequestId::from(n),
                lsp_types::NumberOrString::String(s) => RequestId::from(s),
            });
        }
        false
    });
    ids.retain(|id| {
        queue
            .iter()
            .any(|message| matches!(message, Message::Request(request) if &request.id == id))
    });
    ids
}

/// Re-analyze documents edited since the last flush: completion metadata,
/// the project typecheck, and diagnostics.
fn flush_pending_analysis(connection: &Connection, state: &mut ServerState) -> Result<(), Box<dyn std::error::Error>> {
    for uri in std::mem::take(&mut state.pending_analysis) {
        let Some(document) = state.documents.get_mut(&uri) else {
            continue;
        };
        let offset = document.text.len().saturating_sub(1);
        if let Some(good) = analyze_for_completions_at(&document.text, Some(offset)) {
            document.last_good = Some(good);
        }
        if let Some(path) = uri_path(&uri) {
            refresh_project(state, &path);
        }
        publish_diagnostics(connection, state, &uri)?;
    }
    Ok(())
}

/// Publish diagnostics for `uri` and every file in the last project
/// typecheck (an imported file's parse error belongs to that file). Files
/// published earlier that are now clean get an empty list.
fn publish_diagnostics(
    connection: &Connection,
    state: &mut ServerState,
    uri: &Uri,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut batch: Vec<(Uri, Vec<Diagnostic>, Option<i32>)> = Vec::new();
    if let Some(document) = state.documents.get(uri) {
        batch.push((
            uri.clone(),
            project_diagnostics(state, uri, document),
            Some(document.version),
        ));
    }
    let own_path = uri_path(uri);
    for (path, messages) in &state.last_typecheck {
        if own_path.as_ref() == Some(path) {
            continue;
        }
        let Some(file_uri) = path_to_uri(path) else {
            continue;
        };
        let open = state.documents.get(&file_uri);
        let text = match open {
            Some(document) => document.text.clone(),
            None => match state
                .project_index
                .as_ref()
                .and_then(|index| index.source_for(path))
            {
                Some(text) => text.to_owned(),
                None => std::fs::read_to_string(path).unwrap_or_default(),
            },
        };
        let diagnostics = messages
            .iter()
            .map(|message| diagnostic(&file_uri, &text, message))
            .collect();
        batch.push((file_uri, diagnostics, open.map(|d| d.version)));
    }
    let mut now_dirty = HashSet::new();
    for (file_uri, diagnostics, version) in batch {
        let key = file_uri.to_string();
        let dirty = !diagnostics.is_empty();
        // Skip clean files nobody saw diagnostics for.
        if !dirty && !state.dirty_uris.contains(&key) && &file_uri != uri {
            continue;
        }
        if dirty {
            now_dirty.insert(key);
        }
        send_diagnostics(connection, file_uri, diagnostics, version)?;
    }
    // Clear files that had diagnostics and are no longer in the result.
    let current: HashSet<String> = state
        .last_typecheck
        .iter()
        .filter_map(|(p, _)| path_to_uri(p).map(|u| u.to_string()))
        .chain(std::iter::once(uri.to_string()))
        .collect();
    for stale in state.dirty_uris.difference(&now_dirty) {
        if current.contains(stale) {
            continue;
        }
        if let Ok(stale_uri) = stale.parse::<Uri>() {
            send_diagnostics(connection, stale_uri, Vec::new(), None)?;
        }
    }
    state.dirty_uris = now_dirty;
    Ok(())
}

fn send_diagnostics(
    connection: &Connection,
    uri: Uri,
    diagnostics: Vec<Diagnostic>,
    version: Option<i32>,
) -> Result<(), Box<dyn std::error::Error>> {
    connection
        .sender
        .send(Message::Notification(Notification::new(
            "textDocument/publishDiagnostics".into(),
            PublishDiagnosticsParams {
                uri,
                diagnostics,
                version,
            },
        )))?;
    Ok(())
}

fn analyze(source: &str) -> Vec<CoilMessage> {
    let ast = match Pratt::default().parse(source) {
        Ok(ast) => ast,
        Err(message) => return vec![message],
    };
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let mut messages = checker.take_messages();
    // Effect declarations are checked on a well-typed file.
    if !messages.iter().any(|m| *m.kind() == MessageKind::ERROR) {
        messages.extend(compiler::effect_declaration_errors(&checker, &ast));
    }
    messages
}

fn lsp_module_roots(workspace: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = compiler::default_module_roots();
    let dot = PathBuf::from(".");
    if !roots.contains(&dot) {
        roots.push(dot);
    }
    let deps = workspace.join(".deps");
    if let Ok(entries) = std::fs::read_dir(deps) {
        for entry in entries.flatten() {
            let src = entry.path().join("src");
            if !src.is_dir() {
                continue;
            }
            if let Ok(rel) = src.strip_prefix(workspace) {
                roots.push(rel.to_path_buf());
            } else {
                roots.push(src);
            }
        }
    }
    for root in extra {
        if !roots.contains(root) {
            roots.push(root.clone());
        }
    }
    roots
}

fn refresh_project(state: &mut ServerState, entry: &Path) {
    let Some(index) = state.project_index.as_mut() else {
        return;
    };
    for (uri, document) in &state.documents {
        if let Some(path) = uri_path(uri) {
            index.apply_open_file(path, document.text.clone());
        }
    }
    state.last_typecheck = index.typecheck_entry(entry);
    state.last_entry = Some(entry.to_path_buf());
}

fn project_diagnostics(state: &ServerState, uri: &Uri, document: &Document) -> Vec<Diagnostic> {
    let path = uri_path(uri);
    let Some(path) = path else {
        return analyze(&document.text)
            .iter()
            .map(|message| diagnostic(uri, &document.text, message))
            .collect();
    };
    if let Some((_, messages)) = state
        .last_typecheck
        .iter()
        .find(|(file, _)| file == &path)
    {
        return messages
            .iter()
            .map(|message| diagnostic(uri, &document.text, message))
            .collect();
    }
    analyze(&document.text)
        .iter()
        .map(|message| diagnostic(uri, &document.text, message))
        .collect()
}

fn diagnostic(uri: &Uri, source: &str, message: &CoilMessage) -> Diagnostic {
    let severity = match message.kind() {
        MessageKind::ERROR => DiagnosticSeverity::ERROR,
        MessageKind::WARNING => DiagnosticSeverity::WARNING,
        MessageKind::INFO => DiagnosticSeverity::INFORMATION,
    };
    let related_information = message
        .labels()
        .iter()
        .map(|label: &Label| DiagnosticRelatedInformation {
            location: Location {
                uri: uri.clone(),
                range: byte_range(source, label.range()),
            },
            message: label.message().to_owned(),
        })
        .collect::<Vec<_>>();
    Diagnostic {
        range: byte_range(source, message.range()),
        severity: Some(severity),
        code: message
            .code()
            .map(|code| lsp_types::NumberOrString::String(code.as_str().to_owned())),
        source: Some("coil".into()),
        message: message.message().to_owned(),
        related_information: (!related_information.is_empty()).then_some(related_information),
        ..Diagnostic::default()
    }
}

fn uri_path(uri: &Uri) -> Option<PathBuf> {
    let raw = uri.to_string();
    let path = raw
        .strip_prefix("file://")
        .map(percent_decode)
        .map(PathBuf::from)
        .or_else(|| {
            raw.starts_with('/')
                .then_some(PathBuf::from(percent_decode(&raw)))
        })?;
    Some(path)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len()
            && let Ok(value) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                out.push(value);
                index += 3;
                continue;
            }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| input.to_owned())
}

fn path_to_uri(path: &Path) -> Option<Uri> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut encoded = String::from("file://");
    for (index, part) in abs.to_string_lossy().split('/').enumerate() {
        if index > 0 {
            encoded.push('/');
        }
        for byte in part.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    encoded.push(byte as char);
                }
                _ => encoded.push_str(&format!("%{byte:02X}")),
            }
        }
    }
    encoded.parse().ok()
}

fn location_for_file(state: &ServerState, path: &Path, name_range: &Range<usize>) -> Option<Location> {
    if let Some((uri, document)) = state.documents.iter().find(|(uri, _)| {
        uri_path(uri).as_deref() == Some(path)
    }) {
        return Some(Location {
            uri: uri.clone(),
            range: byte_range(&document.text, name_range),
        });
    }
    let source = state
        .project_index
        .as_ref()
        .and_then(|index| index.source_for(path))
        .map(str::to_owned)
        .or_else(|| std::fs::read_to_string(path).ok())?;
    Some(Location {
        uri: path_to_uri(path)?,
        range: byte_range(&source, name_range),
    })
}

fn goto_definitions(state: &ServerState, uri: &Uri, position: Position) -> Vec<Location> {
    let Some(document) = state.documents.get(uri) else {
        return Vec::new();
    };
    let Some(offset) = position_to_byte(&document.text, position) else {
        return Vec::new();
    };
    let Some(range) = word_range(&document.text, offset) else {
        return Vec::new();
    };
    let name = document.text[range.clone()].to_owned();
    let Some(file_path) = uri_path(uri) else {
        return local_definitions(state, &name);
    };

    if let Some(index) = &state.project_index {
        let defs = index.resolve_definition(&file_path, range.clone(), &name);
        let resolved: Vec<_> = defs
            .into_iter()
            .filter_map(|(path, name_range)| location_for_file(state, &path, &name_range))
            .collect();
        if !resolved.is_empty() {
            return resolved;
        }
    }
    local_definitions(state, &name)
}

fn local_definitions(state: &ServerState, name: &str) -> Vec<Location> {
    let mut locations = Vec::new();
    for (document_uri, open_document) in &state.documents {
        let index = SymbolIndex::from_source(
            uri_path(document_uri).unwrap_or_default(),
            &open_document.text,
        );
        for definition in index.definitions(name) {
            locations.push(Location {
                uri: document_uri.clone(),
                range: byte_range(&open_document.text, &definition.name_range),
            });
        }
    }
    locations
}

fn identifier_name(document: &Document, position: Position) -> Option<String> {
    let offset = position_to_byte(&document.text, position)?;
    let range = word_range(&document.text, offset)?;
    Some(document.text[range].to_owned())
}

fn identifier_locations(
    state: &ServerState,
    uri: &Uri,
    position: Position,
    include_declaration: bool,
) -> Vec<Location> {
    let Some(document) = state.documents.get(uri) else {
        return Vec::new();
    };
    let Some(name) = identifier_name(document, position) else {
        return Vec::new();
    };
    // Function locals resolve through lexical scopes, never by name.
    if let Some(offset) = position_to_byte(&document.text, position)
        && let Some(binding) = compiler::binding_at(&document.text, offset)
    {
        return binding
            .occurrences()
            .skip(usize::from(!include_declaration))
            .map(|range| Location {
                uri: uri.clone(),
                range: byte_range(&document.text, range),
            })
            .collect();
    }
    let mut locations = Vec::new();
    let mut seen = HashSet::new();

    let mut push = |location: Location| {
        let key = (
            location.uri.to_string(),
            location.range.start.line,
            location.range.start.character,
            location.range.end.line,
            location.range.end.character,
        );
        if seen.insert(key) {
            locations.push(location);
        }
    };

    let mut visit_index = |path: &Path, source: &str, index: &SymbolIndex| {
        // A local that happens to share the global's name is not a use of it.
        let local_starts: HashSet<usize> = compiler::local_bindings(source)
            .iter()
            .filter(|binding| binding.name == name)
            .flat_map(|binding| binding.occurrences().map(|r| r.start).collect::<Vec<_>>())
            .collect();
        if include_declaration {
            for definition in index.definitions(&name) {
                if let Some(location) = location_for_file(state, path, &definition.name_range)
                    .or_else(|| {
                        path_to_uri(path).map(|uri| Location {
                            uri,
                            range: byte_range(source, &definition.name_range),
                        })
                    })
                {
                    push(location);
                }
            }
        }
        for site in index.references(&name) {
            if local_starts.contains(&site.range.start) {
                continue;
            }
            if let Some(location) = location_for_file(state, path, &site.range) {
                push(location);
            }
        }
    };

    if let Some(project) = &state.project_index {
        for path in project.indexed_paths() {
            if let Some(source) = project.source_for(path)
                && let Some(index) = project.symbols_for(path) {
                    visit_index(path, source, index);
                }
        }
    }

    for (document_uri, open_document) in &state.documents {
        let path = uri_path(document_uri).unwrap_or_default();
        if state
            .project_index
            .as_ref()
            .is_some_and(|index| index.source_for(&path).is_some())
        {
            continue;
        }
        let index = SymbolIndex::from_source(path.clone(), &open_document.text);
        visit_index(&path, &open_document.text, &index);
    }
    locations
}

fn identifier_highlights(
    state: &ServerState,
    uri: &Uri,
    position: Position,
) -> Vec<DocumentHighlight> {
    identifier_locations(state, uri, position, true)
        .into_iter()
        .filter(|location| &location.uri == uri)
        .map(|location| DocumentHighlight {
            range: location.range,
            kind: None,
        })
        .collect()
}

fn rename_identifier(
    state: &ServerState,
    uri: &Uri,
    position: Position,
    new_name: &str,
) -> lsp_types::WorkspaceEdit {
    let locations = identifier_locations(state, uri, position, true);
    // Group by URI text, then parse back into the `WorkspaceEdit` map.
    let mut by_uri: HashMap<String, Vec<TextEdit>> = HashMap::new();
    for location in locations {
        by_uri
            .entry(location.uri.to_string())
            .or_default()
            .push(TextEdit {
                range: location.range,
                new_text: new_name.to_owned(),
            });
    }
    lsp_types::WorkspaceEdit {
        changes: Some(
            by_uri
                .into_iter()
                .map(|(uri, edits)| {
                    let uri: Uri = uri.parse().expect("uri roundtrip");
                    (uri, edits)
                })
                .collect(),
        ),
        document_changes: None,
        change_annotations: None,
    }
}

fn workspace_symbols(state: &ServerState, query: &str) -> Vec<SymbolInformation> {
    let query = query.to_lowercase();
    let mut symbols = Vec::new();
    let mut seen = HashSet::new();

    let mut push_def = |name: &str, kind: compiler::SymbolKind, path: &Path, range: &Range<usize>| {
        if !query.is_empty() && !name.to_lowercase().contains(&query) {
            return;
        }
        let Some(location) = location_for_file(state, path, range) else {
            return;
        };
        let key = (location.uri.to_string(), name.to_owned(), location.range.start.line);
        if !seen.insert(key) {
            return;
        }
        symbols.push(SymbolInformation {
            name: name.to_owned(),
            kind: match kind {
                compiler::SymbolKind::Function => lsp_types::SymbolKind::FUNCTION,
                compiler::SymbolKind::Class => lsp_types::SymbolKind::CLASS,
                compiler::SymbolKind::Enum => lsp_types::SymbolKind::ENUM,
                compiler::SymbolKind::TypeAlias => lsp_types::SymbolKind::TYPE_PARAMETER,
                compiler::SymbolKind::Variable => lsp_types::SymbolKind::VARIABLE,
                compiler::SymbolKind::Namespace => lsp_types::SymbolKind::NAMESPACE,
                compiler::SymbolKind::Method => lsp_types::SymbolKind::METHOD,
                compiler::SymbolKind::Macro => lsp_types::SymbolKind::FUNCTION,
            },
            tags: None,
            deprecated: None,
            location,
            container_name: None,
        });
    };

    if let Some(project) = &state.project_index {
        for path in project.indexed_paths() {
            if let Some(index) = project.symbols_for(path) {
                for definition in index.all_definitions() {
                    push_def(
                        &definition.name,
                        definition.kind,
                        path,
                        &definition.name_range,
                    );
                }
            }
        }
    }

    for (uri, document) in &state.documents {
        let path = uri_path(uri).unwrap_or_default();
        if state
            .project_index
            .as_ref()
            .is_some_and(|index| index.source_for(&path).is_some())
        {
            continue;
        }
        let index = SymbolIndex::from_source(path.clone(), &document.text);
        for definition in index.all_definitions() {
            push_def(
                &definition.name,
                definition.kind,
                &path,
                &definition.name_range,
            );
        }
    }
    symbols
}

fn format_requested_range(source: &str, range: Range<usize>) -> Option<Vec<TextEdit>> {
    let formatted = parser::format_source(source).ok()?;
    if formatted == source {
        return Some(Vec::new());
    }
    let Ok((_, original_root)) = Pratt::default().parse(source) else {
        return Some(vec![TextEdit {
            range: full_range(source),
            new_text: formatted,
        }]);
    };
    let Ok((_, formatted_root)) = Pratt::default().parse(&formatted) else {
        return Some(vec![TextEdit {
            range: full_range(source),
            new_text: formatted,
        }]);
    };
    let Expression::Program(original_items) = original_root.as_ref() else {
        return Some(vec![TextEdit {
            range: full_range(source),
            new_text: formatted,
        }]);
    };
    let Expression::Program(formatted_items) = formatted_root.as_ref() else {
        return Some(vec![TextEdit {
            range: full_range(source),
            new_text: formatted,
        }]);
    };
    if original_items.len() != formatted_items.len() {
        return Some(vec![TextEdit {
            range: full_range(source),
            new_text: formatted,
        }]);
    }
    let mut edits = Vec::new();
    for (original, formatted_item) in original_items.iter().zip(formatted_items.iter()) {
        let original_span = original.0.start..original.0.end;
        if !ranges_overlap(&original_span, &range) && !range.contains(&original_span.start) {
            continue;
        }
        let new_text = formatted[formatted_item.0.start..formatted_item.0.end].to_owned();
        if source.get(original_span.clone()) != Some(new_text.as_str()) {
            edits.push(TextEdit {
                range: byte_range(source, &original_span),
                new_text,
            });
        }
    }
    if edits.is_empty() && range == (0..source.len()) {
        edits.push(TextEdit {
            range: full_range(source),
            new_text: formatted,
        });
    }
    Some(edits)
}

fn selection_ranges(source: &str, positions: &[Position]) -> Vec<SelectionRange> {
    positions
        .iter()
        .filter_map(|position| {
            let offset = position_to_byte(source, *position)?;
            let mut spans = if let Ok(ast) = Pratt::default().parse(source) {
                spans_containing(&ast, offset)
            } else {
                Vec::new()
            };
            if spans.is_empty() {
                spans.push(0..source.len());
            }
            let mut parent = None;
            for span in spans.iter().rev() {
                parent = Some(SelectionRange {
                    range: byte_range(source, span),
                    parent: parent.map(Box::new),
                });
            }
            parent
        })
        .collect()
}

fn document_symbols(source: &str) -> Vec<DocumentSymbol> {
    let Ok((_, root)) = Pratt::default().parse(source) else {
        return Vec::new();
    };
    let Expression::Program(items) = root.as_ref() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| symbol_for(source, item))
        .collect()
}

fn symbol_for(source: &str, item: &Output<'_>) -> Option<DocumentSymbol> {
    let (span, expression) = item;
    let (name, kind) = match expression.as_ref() {
        Expression::Function { name, .. } => (*name, lsp_types::SymbolKind::FUNCTION),
        Expression::Class { name, .. } => (*name, lsp_types::SymbolKind::CLASS),
        Expression::TypeAlias { name, .. } => (*name, lsp_types::SymbolKind::TYPE_PARAMETER),
        Expression::EnumDecl { name, .. } => (*name, lsp_types::SymbolKind::ENUM),
        Expression::StaticDecl { name, .. } => (*name, lsp_types::SymbolKind::VARIABLE),
        Expression::AttrDecl { name, .. } => (*name, lsp_types::SymbolKind::METHOD),
        Expression::DeriveDecl { name, .. } | Expression::FnMacroDecl { name, .. } => {
            (*name, lsp_types::SymbolKind::FUNCTION)
        }
        Expression::Use { name, alias, .. } => (
            alias.as_deref().unwrap_or(name),
            lsp_types::SymbolKind::NAMESPACE,
        ),
        _ => return None,
    };
    let range = byte_range(source, &(span.start..span.end));
    #[allow(deprecated)]
    Some(DocumentSymbol {
        name: name.to_owned(),
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: None,
    })
}

#[derive(Clone)]
struct CompletionCandidate {
    label: String,
    kind: CompletionItemKind,
    detail: Option<String>,
    documentation: Option<String>,
    parameter_names: Vec<String>,
}

fn analyze_for_completions(source: &str) -> Option<GoodAnalysis> {
    build_completion_index(source).map(|candidates| GoodAnalysis { candidates })
}

/// Analyze `source`, trying cursor-local sanitization when the buffer does not
/// parse (common while typing an identifier before `;`).
fn analyze_for_completions_at(source: &str, offset: Option<usize>) -> Option<GoodAnalysis> {
    analyze_for_completions(source).or_else(|| {
        let offset = offset?;
        sanitize_variants(source, offset)
            .into_iter()
            .find_map(|sanitized| analyze_for_completions(&sanitized))
    })
}

fn build_completion_index(source: &str) -> Option<HashMap<String, CompletionCandidate>> {
    let ast = Pratt::default().parse(source).ok()?;
    let mut by_label: HashMap<String, CompletionCandidate> = HashMap::new();
    collect_decl_candidates(ast.1.as_ref(), &mut by_label);
    let virtual_candidates = virtual_completion_candidates(ast.1.as_ref());
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    for (name, scheme) in checker.env().top().into_iter().flat_map(|frame| frame.bindings()) {
        let detail = format_ty_for_diag(checker.subst(), &scheme.ty);
        by_label
            .entry(name.to_owned())
            .and_modify(|candidate| {
                if candidate.detail.is_none() {
                    candidate.detail = Some(detail.clone());
                }
                if matches!(
                    candidate.kind,
                    CompletionItemKind::KEYWORD | CompletionItemKind::TEXT
                ) {
                    candidate.kind = completion_kind_for_ty(&scheme.ty);
                }
            })
            .or_insert(CompletionCandidate {
                label: name.to_owned(),
                kind: completion_kind_for_ty(&scheme.ty),
                detail: Some(detail),
                documentation: None,
                parameter_names: Vec::new(),
            });
    }
    for name in checker.env().visible_names() {
        by_label.entry(name.clone()).or_insert(CompletionCandidate {
            label: name,
            kind: CompletionItemKind::VARIABLE,
            detail: None,
            documentation: None,
            parameter_names: Vec::new(),
        });
    }
    for (name, (kind, documentation)) in virtual_candidates {
        by_label
            .entry(name.clone())
            .and_modify(|candidate| {
                if candidate.documentation.is_none() {
                    candidate.documentation = Some(documentation.clone());
                }
                candidate.kind = kind;
            })
            .or_insert(CompletionCandidate {
                label: name,
                kind,
                detail: None,
                documentation: Some(documentation),
                parameter_names: Vec::new(),
            });
    }
    Some(by_label)
}

fn completions(document: &Document, position: Position) -> Vec<CompletionItem> {
    let offset = position_to_byte(&document.text, position);
    let prefix = offset
        .and_then(|offset| word_range(&document.text, offset))
        .map(|range| document.text[range.start..range.end.min(document.text.len())].to_owned())
        .unwrap_or_default();

    let qualifier = offset.and_then(|offset| qualifier_before(&document.text, offset));

    let mut by_label: HashMap<String, CompletionCandidate> = HashMap::new();
    for keyword in coil_keywords().iter().chain(highlight_keywords()) {
        by_label.insert(
            (*keyword).into(),
            CompletionCandidate {
                label: (*keyword).into(),
                kind: CompletionItemKind::KEYWORD,
                detail: Some("keyword".into()),
                documentation: None,
                parameter_names: Vec::new(),
            },
        );
    }

    // Prefer a fresh analysis; if the buffer is mid-edit and won't parse, fall
    // back to sanitized placeholders and finally the last good snapshot.
    let semantic = analyze_for_completions_at(&document.text, offset)
        .map(|good| good.candidates)
        .or_else(|| document.last_good.as_ref().map(|good| good.candidates.clone()));

    if let Some(semantic) = semantic {
        for (label, candidate) in semantic {
            by_label.insert(label, candidate);
        }
    }

    if let Some((module, "::")) = qualifier.as_ref().map(|(name, sep)| (name.as_str(), *sep)) {
        let modules = VirtualModules::new();
        if let Some(exports) = modules.resolve_glob(&[module.to_owned()]) {
            for export in exports {
                let docs = builtin_documentation(&[module.to_owned()], export.short_name(), export);
                by_label.entry(export.short_name().to_owned()).or_insert(
                    CompletionCandidate {
                        label: export.short_name().to_owned(),
                        kind: virtual_completion_kind(export),
                        detail: export.host_registry().map(|reg| format!("HostInvoke `{reg}`")),
                        documentation: Some(docs),
                        parameter_names: Vec::new(),
                    },
                );
            }
        }
    }

    let mut items: Vec<CompletionItem> = by_label
        .into_values()
        .filter(|candidate| {
            if let Some((qual, sep)) = &qualifier {
                return completion_matches_qualifier(candidate, qual, sep);
            }
            prefix.is_empty() || candidate.label.to_lowercase().starts_with(&prefix.to_lowercase())
        })
        .map(|candidate| {
            let insert_label = qualifier
                .as_ref()
                .and_then(|(qual, sep)| {
                    candidate
                        .label
                        .strip_prefix(&format!("{qual}{sep}"))
                        .or_else(|| candidate.label.strip_prefix(&format!("{qual}.")))
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| candidate.label.clone());
            let is_function = candidate.kind == CompletionItemKind::FUNCTION;
            CompletionItem {
                label: candidate.label.clone(),
                kind: Some(candidate.kind),
                detail: candidate.detail,
                documentation: candidate.documentation.map(|text| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: text,
                    })
                }),
                insert_text: Some(if is_function {
                    function_snippet(&insert_label, &candidate.parameter_names)
                } else {
                    insert_label
                }),
                insert_text_format: is_function.then_some(InsertTextFormat::SNIPPET),
                command: is_function.then(|| Command {
                    title: "Trigger parameter hints".into(),
                    command: "editor.action.triggerParameterHints".into(),
                    arguments: None,
                }),
                ..CompletionItem::default()
            }
        })
        .collect();
    items.sort_by(|left, right| left.label.cmp(&right.label));
    items
}

/// Placeholder buffers that often restore a parse while the user is mid-ident.
///
/// Expr-statements require a trailing `;`, so a bare replacement like `_` still
/// fails; try both value and statement forms.
fn sanitize_variants(source: &str, offset: usize) -> Vec<String> {
    let mut variants = Vec::new();
    if qualifier_before(source, offset).is_some() {
        let at = offset.min(source.len());
        variants.push(format!("{}x;{}", &source[..at], &source[at..]));
        variants.push(format!("{}x{}", &source[..at], &source[at..]));
        variants.push(format!("{}Red;{}", &source[..at], &source[at..]));
    }
    let Some(range) = incomplete_ident_near(source, offset) else {
        return variants;
    };
    let before = &source[..range.start];
    let after = &source[range.end..];
    variants.extend([
        format!("{before}0;{after}"),
        format!("{before}0{after}"),
        format!("{before}true;{after}"),
        format!("{before}{after}"),
    ]);
    variants
}

/// Identifier touching `offset`, or the nearest preceding identifier when the
/// cursor sits on whitespace/punctuation (common at EOF while typing).
fn incomplete_ident_near(source: &str, offset: usize) -> Option<Range<usize>> {
    if let Some(range) = word_range(source, offset) {
        return Some(range);
    }
    let bytes = source.as_bytes();
    let mut cursor = offset.min(bytes.len());
    while cursor > 0 {
        let byte = bytes[cursor - 1];
        if byte.is_ascii_whitespace()
            || matches!(
                byte,
                b'{' | b'}' | b'(' | b')' | b'[' | b']' | b',' | b';' | b':' | b'.'
            )
        {
            cursor -= 1;
            continue;
        }
        break;
    }
    word_range(source, cursor)
}

fn completion_kind_for_ty(ty: &compiler::Ty) -> CompletionItemKind {
    match semantic_token_type_for_ty(ty) {
        TOKEN_FUNCTION => CompletionItemKind::FUNCTION,
        TOKEN_TYPE => CompletionItemKind::CLASS,
        _ => CompletionItemKind::VARIABLE,
    }
}

fn semantic_token_type_for_ty(ty: &compiler::Ty) -> u32 {
    match ty {
        compiler::Ty::Fun(_, _) | compiler::Ty::Forall { .. } => TOKEN_FUNCTION,
        compiler::Ty::Con(name) if name.chars().next().is_some_and(|c| c.is_uppercase()) => {
            TOKEN_TYPE
        }
        compiler::Ty::Constructor { .. } => TOKEN_TYPE,
        _ => TOKEN_VARIABLE,
    }
}

fn collect_decl_candidates(expression: &Expression<'_>, out: &mut HashMap<String, CompletionCandidate>) {
    match expression {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            for (_, item) in items {
                collect_decl_candidates(item, out);
            }
        }
        Expression::Function {
            name, docs, args, body, ..
        } => {
            insert_decl_candidate(out, name, CompletionItemKind::FUNCTION, docs);
            if let Some(candidate) = out.get_mut(*name) {
                candidate.parameter_names = function_parameter_names(args);
            }
            if let Some(parameters) = parameter_docs_markdown(args)
                && let Some(candidate) = out.get_mut(*name) {
                    let base = candidate.documentation.take().unwrap_or_default();
                    candidate.documentation = Some(format!("{base}{parameters}"));
                }
            collect_decl_candidates(args.1.as_ref(), out);
            if let Some(body) = body {
                collect_decl_candidates(body.1.as_ref(), out);
            }
        }
        Expression::Class { name, docs, fields, .. } => {
            insert_decl_candidate(out, name, CompletionItemKind::CLASS, docs);
            for (_, field) in fields {
                collect_decl_candidates(field, out);
            }
        }
        Expression::EnumDecl { name, docs, variants, .. } => {
            insert_decl_candidate(out, name, CompletionItemKind::ENUM, docs);
            for (_, variant) in variants {
                let Expression::EnumVariant {
                    name: variant_name,
                    docs: variant_docs,
                    ..
                } = variant.as_ref()
                else {
                    continue;
                };
                insert_decl_candidate(
                    out,
                    &format!("{name}.{variant_name}"),
                    CompletionItemKind::ENUM_MEMBER,
                    variant_docs,
                );
            }
        }
        Expression::TypeAlias { name, docs, .. } => {
            insert_decl_candidate(out, name, CompletionItemKind::TYPE_PARAMETER, docs);
        }
        Expression::StaticDecl { name, .. } => {
            insert_decl_candidate(out, name, CompletionItemKind::VARIABLE, &[]);
        }
        Expression::Variable(name, _) => {
            insert_decl_candidate(out, name, CompletionItemKind::VARIABLE, &[]);
        }
        Expression::Argument { name, docs, .. } => {
            insert_decl_candidate(out, name, CompletionItemKind::VARIABLE, docs);
        }
        Expression::Field { name, docs, .. } => {
            if let Expression::Identifier(field_name) = name.1.as_ref() {
                insert_decl_candidate(out, field_name, CompletionItemKind::FIELD, docs);
            }
        }
        Expression::Method(_, inner) => collect_decl_candidates(inner.1.as_ref(), out),
        Expression::Implementation { methods, .. } => {
            for (_, method) in methods {
                collect_decl_candidates(method, out);
            }
        }
        Expression::AttrDecl { name, docs, .. }
        | Expression::FnMacroDecl { name, docs, .. } => {
            insert_decl_candidate(out, name, CompletionItemKind::FUNCTION, docs);
        }
        Expression::DeriveDecl { name, docs, .. } => {
            insert_decl_candidate(out, name, CompletionItemKind::FUNCTION, docs);
        }
        _ => {}
    }
}

fn insert_decl_candidate(
    out: &mut HashMap<String, CompletionCandidate>,
    name: &str,
    kind: CompletionItemKind,
    docs: &[&str],
) {
    let documentation = docs_markdown(docs);
    out.entry(name.to_owned())
        .and_modify(|candidate| {
            candidate.kind = kind;
            if candidate.documentation.is_none() {
                candidate.documentation = documentation.clone();
            }
        })
        .or_insert(CompletionCandidate {
            label: name.to_owned(),
            kind,
            detail: None,
            documentation,
            parameter_names: Vec::new(),
        });
}

fn docs_markdown(docs: &[&str]) -> Option<String> {
    if docs.is_empty() {
        return None;
    }
    Some(docs.join("\n"))
}

fn virtual_completion_candidates(
    expression: &Expression<'_>,
) -> HashMap<String, (CompletionItemKind, String)> {
    let mut candidates = HashMap::new();
    let modules = VirtualModules::new();
    for module in ["prelude", "prelude::ops", "prelude::test", "prelude::math"] {
        let path = module
            .split("::")
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if let Some(exports) = modules.resolve_glob(&path) {
            for export in exports {
                insert_virtual_candidate(
                    &mut candidates,
                    export.short_name().to_owned(),
                    &path,
                    export,
                );
            }
        }
    }
    collect_virtual_candidates(
        expression,
        &modules,
        &mut candidates,
    );
    candidates
}

fn collect_virtual_candidates(
    expression: &Expression<'_>,
    modules: &VirtualModules,
    out: &mut HashMap<String, (CompletionItemKind, String)>,
) {
    match expression {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            for (_, item) in items {
                collect_virtual_candidates(item, modules, out);
            }
        }
        Expression::Use { path, name, alias } => {
            if name == "*" {
                if let Some(exports) = modules.resolve_glob(path) {
                    for export in exports {
                        insert_virtual_candidate(
                            out,
                            export.short_name().to_owned(),
                            path,
                            export,
                        );
                    }
                }
            } else if let Some(export) = modules.resolve_item(path, name) {
                insert_virtual_candidate(
                    out,
                    alias.as_deref().unwrap_or(name).to_owned(),
                    path,
                    &export,
                );
            }
        }
        Expression::Function { body: Some(body), .. } => {
            collect_virtual_candidates(body.1.as_ref(), modules, out);
        }
        Expression::Method(_, inner) => {
            collect_virtual_candidates(inner.1.as_ref(), modules, out);
        }
        Expression::Implementation { methods, .. } => {
            for (_, method) in methods {
                collect_virtual_candidates(method, modules, out);
            }
        }
        _ => {}
    }
}

fn qualifier_before(source: &str, offset: usize) -> Option<(String, &'static str)> {
    let bytes = source.as_bytes();
    let mut cursor = offset.min(bytes.len());
    while cursor > 0 && bytes[cursor - 1].is_ascii_whitespace() {
        cursor -= 1;
    }
    if cursor >= 2 && bytes[cursor - 1] == b':' && bytes[cursor - 2] == b':' {
        let range = word_range(source, cursor - 2)?;
        return Some((source[range].to_owned(), "::"));
    }
    if cursor >= 1 && bytes[cursor - 1] == b'.' {
        let range = word_range(source, cursor - 1)?;
        return Some((source[range].to_owned(), "."));
    }
    None
}

fn completion_matches_qualifier(
    candidate: &CompletionCandidate,
    qual: &str,
    sep: &str,
) -> bool {
    if candidate.label.starts_with(&format!("{qual}{sep}"))
        || candidate.label.starts_with(&format!("{qual}."))
    {
        return true;
    }
    if sep == "::" {
        return VirtualModules::new()
            .resolve_item(&[qual.to_owned()], &candidate.label)
            .is_some();
    }
    false
}

fn virtual_completion_kind(export: &BuiltinExport) -> CompletionItemKind {
    match export {
        BuiltinExport::Enum { .. } => CompletionItemKind::ENUM,
        BuiltinExport::TypeClass { .. } => CompletionItemKind::INTERFACE,
        BuiltinExport::FfiTag { .. } => CompletionItemKind::ENUM_MEMBER,
        BuiltinExport::OpaqueType { .. } => CompletionItemKind::CLASS,
        BuiltinExport::FfiFn { .. }
        | BuiltinExport::Fn { .. }
        | BuiltinExport::IoFn { .. }
        | BuiltinExport::StringFn { .. }
        | BuiltinExport::ThreadFn { .. }
        | BuiltinExport::GcFn { .. }
        | BuiltinExport::HostFn { .. } => CompletionItemKind::FUNCTION,
    }
}

fn insert_virtual_candidate(
    out: &mut HashMap<String, (CompletionItemKind, String)>,
    name: String,
    path: &[String],
    export: &BuiltinExport,
) {
    let kind = match export {
        BuiltinExport::Enum { .. } => CompletionItemKind::ENUM,
        BuiltinExport::TypeClass { .. } => CompletionItemKind::INTERFACE,
        BuiltinExport::FfiTag { .. } => CompletionItemKind::ENUM_MEMBER,
        BuiltinExport::OpaqueType { .. } => CompletionItemKind::CLASS,
        BuiltinExport::FfiFn { .. }
        | BuiltinExport::Fn { .. }
        | BuiltinExport::IoFn { .. }
        | BuiltinExport::StringFn { .. }
        | BuiltinExport::ThreadFn { .. }
        | BuiltinExport::GcFn { .. }
        | BuiltinExport::HostFn { .. } => CompletionItemKind::FUNCTION,
    };
    out.entry(name.clone())
        .or_insert_with(|| (kind, builtin_documentation(path, &name, export)));
}

const WEBSITE_DOCS: &str =
    "https://github.com/ardax-corp/coil-website/blob/main/src/content/docs";

fn builtin_documentation(path: &[String], name: &str, export: &BuiltinExport) -> String {
    let module = path.join("::");
    let doc_path = match module.as_str() {
        "prelude" => "references/option-result.md",
        "prelude::ops" => "references/types.md",
        "prelude::test" => "references/assert.md",
        "prelude::math" => "references/math.md",
        "io" => "references/io.md",
        "io::fs" => "references/io-fs.md",
        "string" => "references/string.md",
        "thread" => "manual/tutorial/11-threads.md",
        "clock" => "references/modules.md",
        "env" => "references/env.md",
        "gc" => "references/gc.md",
        "ffi" | "ffi::types" => "references/ffi.md",
        _ => "references/modules.md",
    };
    let description = builtin_description(&module, name, export);
    format!("{description}\n\n[Read the `{module}` reference]({WEBSITE_DOCS}/{doc_path}).")
}

fn builtin_description(module: &str, name: &str, export: &BuiltinExport) -> String {
    
    match (module, name) {
        ("io", "stdin") => "Returns a stream connected to standard input.".into(),
        ("io", "stdout") => "Returns a stream connected to standard output.".into(),
        ("io", "stderr") => "Returns a stream connected to standard error.".into(),
        ("io", "open") => "Opens a filesystem path as a stream.".into(),
        ("io", "close") => "Closes a stream and releases its underlying handle.".into(),
        ("io", "read") => {
            "Reads available bytes from a stream without busy-spinning; `None` indicates EOF."
                .into()
        }
        ("io", "write") => "Writes bytes to a stream and reports the number written.".into(),
        ("io", "wait_readable") => "Parks until the stream is readable (yields inside a coroutine).".into(),
        ("io", "wait_writable") => "Parks until the stream is writable (yields inside a coroutine).".into(),
        ("io", "await_readable") => "Old name of `wait_readable`.".into(),
        ("io", "await_writable") => "Old name of `wait_writable`.".into(),
        ("io", "drive") => "Polls async IO waiters once; returns newly-ready count.".into(),
        ("io", "wait_ready") => "Blocks until any registered async IO waiter is ready; returns newly-ready count.".into(),
        ("io", "from_bytes") | ("string", "from_bytes") => {
            "Decodes UTF-8 bytes into a string.".into()
        }
        ("io", "to_bytes") | ("string", "to_bytes") => {
            "Encodes a string as UTF-8 bytes.".into()
        }
        ("string", "format") => {
            "Formats values using Coil's `%` format specifiers.".into()
        }
        ("io::net::tcp", "connect") => "Connects to a TCP endpoint.".into(),
        ("io::net::tcp", "connect_timeout") => {
            "Connects to a TCP endpoint with an absolute timeout.".into()
        }
        ("io::net::tcp", "listen") => "Creates a TCP listener on an address.".into(),
        ("io::net::tcp", "accept") => "Accepts the next pending TCP connection.".into(),
        ("io::net::tcp", "peer_addr") => "Returns the remote TCP address.".into(),
        ("io::net::tcp", "local_addr") => "Returns the local TCP address.".into(),
        ("io::net::tcp", "set_nodelay") => "Enables or disables TCP_NODELAY.".into(),
        ("io::net::tcp", "shutdown") => "Shuts down one or both directions of a TCP stream.".into(),
        ("io::net::udp", "bind") => "Binds a UDP socket to a local address.".into(),
        ("io::net::udp", "connect") => "Creates a UDP socket connected to a peer.".into(),
        ("io::net::udp", "send_to") => "Sends a datagram to an explicit UDP peer.".into(),
        ("io::net::udp", "recv_from") => "Receives a UDP datagram without waiting.".into(),
        ("io::net::udp", "local_port") => "Returns the local UDP port.".into(),
        ("prelude::test", "assert") => "Checks a condition and returns a result instead of aborting.".into(),
        ("prelude", "ord") => "Returns the first UTF-8 code unit of a string.".into(),
        ("prelude", "char") => "Builds a one-code-unit string from a byte.".into(),
        ("prelude::math", "dot") => "Computes the dot product of two numeric vectors.".into(),
        ("prelude::math", "matmul") => "Multiplies two compatible matrices.".into(),
        ("prelude::math", "cross") => "Computes the three-dimensional cross product.".into(),
        ("prelude::math", "matrix") => "Constructs a matrix from nested static rows.".into(),
        ("prelude::math", "atan") => "Arc tangent of a float (radians).".into(),
        ("prelude::math", "atan2") => "Two-argument arc tangent `atan2(y, x)` (radians).".into(),
        ("prelude::math", "asin") => "Arc sine of a float (radians).".into(),
        ("prelude::math", "acos") => "Arc cosine of a float (radians).".into(),
        ("prelude::math", "log10") => "Base-10 logarithm of a float.".into(),
        ("prelude::math", "log2") => "Base-2 logarithm of a float.".into(),
        ("prelude::math", "cbrt") => "Cube root of a float.".into(),
        ("prelude::math", "rem") => {
            "Float remainder (`f64::rem` / C `fmod`); sign follows the dividend.".into()
        }
        ("prelude::math", "sinh") => "Hyperbolic sine of a float.".into(),
        ("prelude::math", "cosh") => "Hyperbolic cosine of a float.".into(),
        ("prelude::math", "tanh") => "Hyperbolic tangent of a float.".into(),
        ("clock", "wall_nanos") => {
            "Wall-clock nanoseconds since the Unix epoch (HostInvoke `clock_wall_nanos`)."
                .into()
        }
        ("clock", "mono_nanos") => {
            "Monotonic clock reading in nanoseconds (HostInvoke `clock_mono_nanos`).".into()
        }
        ("clock", "sleep_ms") => {
            "Sleeps the current thread for milliseconds (HostInvoke `clock_sleep_ms`).".into()
        }
        ("ffi", "dload") => "Loads a dynamic library and returns a handle.".into(),
        ("ffi", "declare") => "Declares an FFI function signature for later invocation.".into(),
        ("ffi", "invoke") => "Invokes a previously declared FFI function.".into(),
        ("env", "args") => "Returns the process command-line arguments.".into(),
        ("env", "var") => "Reads an environment variable.".into(),
        ("env", "set_var") => "Sets an environment variable.".into(),
        ("env", "remove_var") => "Removes an environment variable.".into(),
        ("env", "cwd") => "Returns the current working directory.".into(),
        ("env", "set_cwd") => "Changes the current working directory.".into(),
        ("env", "exit") => "Terminates the process with an exit code.".into(),
        ("env", "exec") => "Executes a process with the supplied arguments.".into(),
        ("prelude", "Option") => "Represents an optional value with `Some` or `None`.".into(),
        ("prelude", "Result") => "Represents success with `Ok` or failure with `Err`.".into(),
        ("thread", "spawn") => "Starts a thread and returns a joinable handle.".into(),
        ("thread", "join") => "Waits for a thread and returns its result.".into(),
        ("thread", "detach") => "Detaches a thread so it can finish independently.".into(),
        ("thread", "channel") => "Creates a sender/receiver channel pair.".into(),
        ("thread", "send") => "Sends a value through a channel.".into(),
        ("thread", "recv") => "Receives the next value from a channel.".into(),
        ("thread", "try_send") => "Attempts a channel send without waiting.".into(),
        ("thread", "try_recv") => "Attempts a channel receive without waiting.".into(),
        ("thread", "close") => "Closes a channel endpoint.".into(),
        ("thread", "mutex") => "Creates a mutex.".into(),
        ("thread", "with_lock") => "Runs a callback while holding a mutex lock.".into(),
        ("thread", "lock") => "Acquires a mutex lock.".into(),
        ("thread", "try_lock") => "Attempts to acquire a mutex without waiting.".into(),
        ("thread", "unlock") => "Releases a mutex lock.".into(),
        ("thread", "rwlock") => "Creates a reader-writer lock.".into(),
        ("thread", "with_read") => "Runs a callback with a read lock.".into(),
        ("thread", "with_write") => "Runs a callback with a write lock.".into(),
        ("thread", "try_read") => "Attempts to acquire a read lock without waiting.".into(),
        ("thread", "try_write") => "Attempts to acquire a write lock without waiting.".into(),
        ("gc", "root") => "Pins a value so the GC keeps it alive (`Root<T>`).".into(),
        ("gc", "unroot") => "Takes the pinned value and clears the `Root`.".into(),
        ("gc", "get") => "Reads the value inside a `Root` without releasing the pin.".into(),
        ("gc", "weak") => "Creates a non-rooting `Weak<T>` handle.".into(),
        ("gc", "upgrade") => "Upgrades a `Weak<T>` to `Option<T>` if the referent is live.".into(),
        ("gc", "heap_bytes") => "Returns the managed heap size in bytes.".into(),
        ("gc", "collect") => "Forces a full GC; returns bytes freed.".into(),
        ("gc", "Root") => "Strong GC pin type constructor (`Root<T>`).".into(),
        ("gc", "Weak") => "Weak GC handle type constructor (`Weak<T>`).".into(),
        _ => match export {
            BuiltinExport::TypeClass { .. } => {
                format!("Provides the `{name}` typeclass used by generic constraints.")
            }
            BuiltinExport::Enum { .. } => {
                format!("Provides the `{name}` builtin enum and its constructors.")
            }
            BuiltinExport::OpaqueType { .. } => {
                format!("Provides the opaque `{name}` handle type.")
            }
            BuiltinExport::FfiTag { .. } => {
                format!("Provides the `{name}` tag used to describe an FFI argument.")
            }
            BuiltinExport::HostFn { registry, .. } => {
                format!("HostInvoke `{registry}` (`{module}::{name}`).")
            }
            _ => format!(
                "Provides the `{name}` operation; see the reference for its signature and behavior."
            ),
        },
    }
}

fn parameter_docs_markdown(args: &Output<'_>) -> Option<String> {
    let Expression::Fragment(items) = args.1.as_ref() else {
        return None;
    };
    let lines = items
        .iter()
        .filter_map(|(_, item)| {
            let Expression::Argument {
                docs,
                ty,
                name,
                ..
            } = item.as_ref()
            else {
                return None;
            };
            if docs.is_empty() {
                return None;
            }
            let ty = ty
                .as_ref()
                .map(|ty| ty.1.to_string())
                .unwrap_or_else(|| "...".into());
            Some(format!("- `{name}` (`{ty}`): {}", docs.join(" ")))
        })
        .collect::<Vec<_>>();
    (!lines.is_empty()).then(|| format!("\n\n**Parameters**\n\n{}", lines.join("\n")))
}

fn function_parameter_names(args: &Output<'_>) -> Vec<String> {
    let Expression::Fragment(items) = args.1.as_ref() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|(_, item)| {
            let Expression::Argument { name, .. } = item.as_ref() else {
                return None;
            };
            Some((*name).to_owned())
        })
        .collect()
}

fn function_snippet(name: &str, parameters: &[String]) -> String {
    if parameters.is_empty() {
        return format!("{name}($0)");
    }
    let args = parameters
        .iter()
        .enumerate()
        .map(|(index, parameter)| format!("${{{}:{parameter}}}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}({args})$0")
}

/// Hover from the project typecheck: cross-file types and docs.
///
/// The checker keeps the span tables of the last checked module (the
/// entry), so re-run the project with this document as entry if needed.
/// `None` when the buffer does not parse; the single-file path handles that.
fn project_hover(state: &mut ServerState, uri: &Uri, position: Position) -> Option<Hover> {
    let path = uri_path(uri)?;
    state.project_index.as_ref()?;
    let text = state.documents.get(uri)?.text.clone();
    let offset = position_to_byte(&text, position)?;
    let range = word_range(&text, offset)?;
    let name = text.get(range.clone())?.to_owned();
    let ast = Pratt::default().parse(&text).ok()?;
    if state.last_entry.as_ref() != Some(&path) {
        refresh_project(state, &path);
    }
    let checker = state.project_index.as_ref()?.checker();
    let ty_text = hover_type(&ast, Some(checker), &text, &name, offset, &range);
    let docs = find_param_docs_for_name(ast.1.as_ref(), &name)
        .or_else(|| find_docs_for_name(ast.1.as_ref(), &name))
        .or_else(|| definition_docs(state, uri, position, &name))
        .or_else(|| {
            virtual_completion_candidates(ast.1.as_ref())
                .remove(&name)
                .map(|(_, docs)| docs)
        });
    if ty_text.is_none() && docs.is_none() {
        return None;
    }
    hover_markup(&text, &name, range, ty_text, docs)
}

/// `///` docs at the definition site of the word under the cursor, which may
/// live in another (possibly unopened) file.
fn definition_docs(state: &ServerState, uri: &Uri, position: Position, name: &str) -> Option<String> {
    goto_definitions(state, uri, position).into_iter().find_map(|location| {
        let path = uri_path(&location.uri)?;
        let source = match state.documents.get(&location.uri) {
            Some(document) => document.text.clone(),
            None => state
                .project_index
                .as_ref()
                .and_then(|index| index.source_for(&path).map(str::to_owned))
                .or_else(|| std::fs::read_to_string(&path).ok())?,
        };
        let ast = Pratt::default().parse(&source).ok()?;
        find_docs_for_name(ast.1.as_ref(), name)
    })
}

/// Type text for `name` at `offset`, most specific source first.
fn hover_type(
    ast: &Output<'_>,
    checker: Option<&Checker>,
    source: &str,
    name: &str,
    offset: usize,
    range: &Range<usize>,
) -> Option<String> {
    let show = |checker: &Checker, ty: &compiler::Ty| format_ty_for_diag(checker.subst(), ty);
    // `let x: T = …` names its own type; `let x = e` has the type of `e`.
    let binding = let_binding_at(ast, name, range.start);
    if let Some((Some(annotation), _)) = &binding {
        return source.get(annotation.clone()).map(|text| text.trim().to_owned());
    }
    let checker = checker?;
    if let Some(ty) = checker.lookup_for_codegen_span(range.start, range.end) {
        return Some(show(checker, &ty));
    }
    if let Some((None, Some(value))) = &binding
        && let Some(ty) = checker.lookup_for_codegen_span(value.start, value.end)
    {
        return Some(show(checker, &ty));
    }
    if let Some(text) = find_parameter_type_for_name(ast.1.as_ref(), name) {
        return Some(text);
    }
    // Member names (`p.sum`) have no node of their own: use the smallest
    // enclosing access / call. Statement spans type as `unit`; skip them.
    let mut enclosing = Vec::new();
    collect_nodes_containing(ast, offset, &mut enclosing);
    enclosing.sort_by_key(|node| node.0.end - node.0.start);
    for node in enclosing {
        if !matches!(
            node.1.as_ref(),
            Expression::Access(..)
                | Expression::OptionalAccess(..)
                | Expression::Call { .. }
                | Expression::QualifiedAccess { .. }
                | Expression::Instantiate(..)
        ) {
            continue;
        }
        if let Some(ty) = checker.lookup_for_codegen_span(node.0.start, node.0.end) {
            return Some(show(checker, &ty));
        }
    }
    checker
        .env()
        .lookup(name)
        .map(|scheme| show(checker, &scheme.ty))
}

/// `(annotation, initializer)` spans of a `let`.
type LetSpans = (Option<Range<usize>>, Option<Range<usize>>);

/// For `let name[: T] [= value]` whose name starts at `name_start`, the
/// annotation and initializer spans.
fn let_binding_at(
    ast: &Output<'_>,
    name: &str,
    name_start: usize,
) -> Option<LetSpans> {
    let mut found = None;
    visit_nodes(ast, &mut |node| {
        if found.is_some() {
            return;
        }
        let Expression::Fragment(items) = node.1.as_ref() else {
            return;
        };
        let Some((span, head)) = items.first() else {
            return;
        };
        let Expression::Variable(var, annotation) = head.as_ref() else {
            return;
        };
        // `let` + space, then the name: the name sits inside the head span
        // and before the annotation / initializer.
        let before_rest = annotation
            .as_ref()
            .map(|a| a.0.start)
            .or_else(|| items.get(1).map(|v| v.0.start))
            .unwrap_or(span.end);
        if *var == name && name_start >= span.start && name_start < before_rest {
            found = Some((
                annotation.as_ref().map(|a| a.0.start..a.0.end),
                items.get(1).map(|v| v.0.start..v.0.end),
            ));
        }
    });
    found
}

/// Pre-order walk of every node.
fn visit_nodes<'a, 'e>(node: &'a Output<'e>, f: &mut dyn FnMut(&'a Output<'e>)) {
    f(node);
    node.1.for_each_child(&mut |child| visit_nodes(child, f));
}

fn collect_nodes_containing<'a, 'e>(node: &'a Output<'e>, offset: usize, out: &mut Vec<&'a Output<'e>>) {
    if offset < node.0.start || offset > node.0.end {
        return;
    }
    out.push(node);
    node.1
        .for_each_child(&mut |child| collect_nodes_containing(child, offset, out));
}

/// `recv.` / `recv.pre` ending at `offset`: receiver path segments
/// (`self.items` → `["self", "items"]`), the typed member prefix, and the
/// byte range of `.pre`.
fn member_access_at(text: &str, offset: usize) -> Option<(Vec<String>, String, Range<usize>)> {
    let bytes = text.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = offset.min(bytes.len());
    let prefix_end = i;
    while i > 0 && is_ident(bytes[i - 1]) {
        i -= 1;
    }
    let prefix = text[i..prefix_end].to_string();
    if i == 0 || bytes[i - 1] != b'.' {
        return None;
    }
    let dot = i - 1;
    let mut segments = Vec::new();
    let mut end = dot;
    loop {
        let mut start = end;
        while start > 0 && is_ident(bytes[start - 1]) {
            start -= 1;
        }
        if start == end || bytes[start].is_ascii_digit() {
            return None;
        }
        segments.push(text[start..end].to_string());
        if start > 0 && bytes[start - 1] == b'.' {
            end = start - 1;
        } else {
            break;
        }
    }
    segments.reverse();
    Some((segments, prefix, dot..prefix_end))
}

/// Class name inside a type's display text (`util::Point`, `Box<int>`).
fn class_of_type_text(text: &str) -> String {
    text.split('<').next().unwrap_or(text).trim().to_string()
}

/// Inlay hints inside `range`: the inferred type after an unannotated
/// `let` name, and parameter names before positional call arguments.
fn inlay_hints(state: &mut ServerState, uri: &Uri, range: LspRange) -> Option<Vec<lsp_types::InlayHint>> {
    let path = uri_path(uri)?;
    let text = state.documents.get(uri)?.text.clone();
    let window = lsp_range_to_byte_range(&text, range).unwrap_or(0..text.len());
    let ast = Pratt::default().parse(&text).ok()?;
    if state.project_index.is_some() && state.last_entry.as_ref() != Some(&path) {
        refresh_project(state, &path);
    }
    // Parameter names of free functions declared here or in the project.
    let mut sources = vec![text.clone()];
    if let Some(index) = &state.project_index {
        for indexed in index.indexed_paths() {
            if indexed != &path
                && let Some(source) = index.source_for(indexed)
            {
                sources.push(source.to_owned());
            }
        }
    }
    let mut params_cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
    let mut params_of = |name: &str| {
        params_cache
            .entry(name.to_owned())
            .or_insert_with(|| sources.iter().find_map(|source| function_parameter_list(source, name)))
            .clone()
    };
    let checker = state.project_index.as_ref().map(|index| index.checker());
    let mut hints = Vec::new();
    let in_window = |at: usize| window.start <= at && at <= window.end;
    if let Some(checker) = checker {
        for (name_end, label) in let_type_hints(&ast, &text, checker) {
            if in_window(name_end) {
                hints.push(lsp_types::InlayHint {
                    position: byte_position(&text, name_end),
                    label: lsp_types::InlayHintLabel::String(format!(": {label}")),
                    kind: Some(lsp_types::InlayHintKind::TYPE),
                    text_edits: None,
                    tooltip: None,
                    padding_left: None,
                    padding_right: None,
                    data: None,
                });
            }
        }
    }
    visit_nodes(&ast, &mut |node| {
        let Expression::Call {
            name,
            args: Some(args),
        } = node.1.as_ref()
        else {
            return;
        };
        let Expression::Identifier(callee) = name.1.as_ref() else {
            return;
        };
        let Some(parameters) = params_of(callee) else {
            return;
        };
        for (arg, parameter) in args.iter().zip(&parameters) {
            if !in_window(arg.0.start) || matches!(arg.1.as_ref(), Expression::NamedArg(..)) {
                continue;
            }
            // `f(count)` for parameter `count` says it already.
            if matches!(arg.1.as_ref(), Expression::Identifier(id) if id == parameter) {
                continue;
            }
            hints.push(lsp_types::InlayHint {
                position: byte_position(&text, arg.0.start),
                label: lsp_types::InlayHintLabel::String(format!("{parameter}:")),
                kind: Some(lsp_types::InlayHintKind::PARAMETER),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: Some(true),
                data: None,
            });
        }
    });
    hints.sort_by_key(|hint| (hint.position.line, hint.position.character));
    Some(hints)
}

/// `(name end, type text)` for every unannotated `let name = value` whose
/// value has a checked type. Skips `_` names and `new C(…)`, which names
/// its type already.
fn let_type_hints(ast: &Output<'_>, text: &str, checker: &Checker) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    visit_nodes(ast, &mut |node| {
        let Expression::Fragment(items) = node.1.as_ref() else {
            return;
        };
        let (Some((head_span, head)), Some((value_span, value))) = (items.first(), items.get(1)) else {
            return;
        };
        let Expression::Variable(name, None) = head.as_ref() else {
            return;
        };
        if name.starts_with('_') || matches!(value.as_ref(), Expression::Instantiate(..)) {
            return;
        }
        let Some(name_start) = text
            .get(head_span.start..head_span.end)
            .and_then(|head| head.find(name))
            .map(|i| head_span.start + i)
        else {
            return;
        };
        let Some(ty) = checker.lookup_for_codegen_span(value_span.start, value_span.end) else {
            return;
        };
        let label = format_ty_for_diag(checker.subst(), &ty);
        if !label.is_empty() && label != "never" {
            out.push((name_start + name.len(), label));
        }
    });
    out
}

/// Quick fixes for `diagnostics` and refactors at `range`.
fn code_actions(
    state: &mut ServerState,
    uri: &Uri,
    range: LspRange,
    diagnostics: &[Diagnostic],
) -> Option<Vec<lsp_types::CodeActionOrCommand>> {
    let path = uri_path(uri)?;
    let text = state.documents.get(uri)?.text.clone();
    let mut actions = Vec::new();
    let edit_action = |title: String, edits: Vec<TextEdit>, diagnostic: Option<&Diagnostic>, kind| {
        lsp_types::CodeActionOrCommand::CodeAction(lsp_types::CodeAction {
            title,
            kind: Some(kind),
            diagnostics: diagnostic.map(|d| vec![d.clone()]),
            edit: Some(lsp_types::WorkspaceEdit {
                changes: Some(HashMap::from([(uri.clone(), edits)])),
                ..lsp_types::WorkspaceEdit::default()
            }),
            is_preferred: diagnostic.map(|_| true),
            ..lsp_types::CodeAction::default()
        })
    };
    for diagnostic in diagnostics {
        let code = match &diagnostic.code {
            Some(lsp_types::NumberOrString::String(code)) => code.as_str(),
            _ => continue,
        };
        let Some(start) = position_to_byte(&text, diagnostic.range.start) else {
            continue;
        };
        match code {
            // Unknown value / function / type: import it from the module
            // that declares it.
            "E0100" | "E0101" | "E0110" => {
                let Some(word) = word_range(&text, start) else {
                    continue;
                };
                let name = &text[word];
                for module in modules_declaring(state, &path, name) {
                    let edit = add_use_edit(&text, &module, name);
                    actions.push(edit_action(
                        format!("Import `{name}` from `{module}`"),
                        vec![edit],
                        Some(diagnostic),
                        lsp_types::CodeActionKind::QUICKFIX,
                    ));
                }
            }
            // Non-exhaustive statement `match`: add an empty catch-all.
            "E0209" => {
                let Some(end) = position_to_byte(&text, diagnostic.range.end) else {
                    continue;
                };
                let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
                let prefix = &text[line_start..start];
                // A value `match` needs a value arm; only fix statements.
                if !prefix.trim().is_empty() {
                    continue;
                }
                let Some(close) = text[start..end].rfind('}').map(|i| start + i) else {
                    continue;
                };
                let close_line = text[..close].rfind('\n').map_or(0, |i| i + 1);
                let insert = if text[close_line..close].trim().is_empty() {
                    // `}` on its own line: new arm line above it.
                    TextEdit {
                        range: byte_range(&text, &(close_line..close_line)),
                        new_text: format!("{prefix}    default => {{}},\n"),
                    }
                } else {
                    TextEdit {
                        range: byte_range(&text, &(close..close)),
                        new_text: " default => {}, ".into(),
                    }
                };
                actions.push(edit_action(
                    "Add `default =>` arm".into(),
                    vec![insert],
                    Some(diagnostic),
                    lsp_types::CodeActionKind::QUICKFIX,
                ));
            }
            _ => {}
        }
    }
    // Refactor: on an attribute line or a `name!(…)` call, replace the file
    // with its macro expansion (what `coil dissect --expand` prints).
    if let Some(cursor) = position_to_byte(&text, range.start) {
        let line_start = text[..cursor].rfind('\n').map_or(0, |i| i + 1);
        let line_end = text[cursor..].find('\n').map_or(text.len(), |i| cursor + i);
        let line = &text[line_start..line_end];
        let on_attr = line.trim_start().starts_with("#[") || has_macro_call(line);
        if on_attr && let Some(index) = state.project_index.as_mut() {
            index.apply_open_file(path.clone(), text.clone());
            let expanded = index
                .pipeline_mut()
                .expanded_source(path.to_str().unwrap_or_default());
            // The session was reset: re-check on the next request.
            state.last_entry = None;
            if let Some(expanded) = expanded.filter(|e| *e != text) {
                actions.push(edit_action(
                    "Expand macros in this file".into(),
                    vec![TextEdit {
                        range: byte_range(&text, &(0..text.len())),
                        new_text: expanded,
                    }],
                    None,
                    lsp_types::CodeActionKind::REFACTOR_REWRITE,
                ));
            }
        }
    }
    // Refactor: spell out the inferred type of the `let` under the cursor.
    if let (Some(cursor), Ok(ast)) = (position_to_byte(&text, range.start), Pratt::default().parse(&text)) {
        if state.project_index.is_some() && state.last_entry.as_ref() != Some(&path) {
            refresh_project(state, &path);
        }
        if let Some(checker) = state.project_index.as_ref().map(|index| index.checker()) {
            let word = word_range(&text, cursor);
            for (name_end, label) in let_type_hints(&ast, &text, checker) {
                if word.as_ref().is_some_and(|w| w.end == name_end) {
                    actions.push(edit_action(
                        format!("Add type annotation `: {label}`"),
                        vec![TextEdit {
                            range: byte_range(&text, &(name_end..name_end)),
                            new_text: format!(": {label}"),
                        }],
                        None,
                        lsp_types::CodeActionKind::REFACTOR_REWRITE,
                    ));
                }
            }
        }
    }
    Some(actions)
}

/// Module paths (`util`, `geo::shapes`) of workspace files that declare a
/// top-level `name`, as `use` would spell them from `from`.
fn modules_declaring(state: &ServerState, from: &Path, name: &str) -> Vec<String> {
    let Some(root) = &state.workspace_root else {
        return Vec::new();
    };
    let module_roots: Vec<PathBuf> = lsp_module_roots(root, &state.extra_roots)
        .into_iter()
        .map(|r| if r.is_absolute() { r } else { root.join(r) })
        .collect();
    let mut files = Vec::new();
    for module_root in &module_roots {
        collect_hy_files(module_root, &mut files);
    }
    let mut modules: Vec<String> = Vec::new();
    for file in files {
        if file == from {
            continue;
        }
        let source = state
            .documents
            .iter()
            .find(|(u, _)| uri_path(u).as_deref() == Some(file.as_path()))
            .map(|(_, d)| d.text.clone())
            .or_else(|| std::fs::read_to_string(&file).ok());
        let Some(source) = source else {
            continue;
        };
        if !document_symbols(&source)
            .iter()
            .any(|symbol| symbol.name == name && symbol.kind != lsp_types::SymbolKind::NAMESPACE)
        {
            continue;
        }
        // Shortest spelling across the module roots.
        let module = module_roots
            .iter()
            .filter_map(|module_root| file.strip_prefix(module_root).ok())
            .map(|rel| {
                rel.with_extension("")
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("::")
            })
            .min_by_key(|module| module.len());
        if let Some(module) = module
            && !modules.contains(&module)
        {
            modules.push(module);
        }
    }
    modules.sort();
    modules
}

/// `.hy` files under `dir`, skipping hidden directories and `target`.
fn collect_hy_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let hidden = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.') || n == "target");
        if path.is_dir() && !hidden {
            collect_hy_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "hy") && !out.contains(&path) {
            out.push(path);
        }
    }
}

/// Edit importing `name` from `module`: joins an existing
/// `use module::{…};`, else adds a line after the last `use`.
fn add_use_edit(text: &str, module: &str, name: &str) -> TextEdit {
    let path: Vec<&str> = module.split("::").collect();
    let items = match Pratt::default().parse(text) {
        Ok((_, root)) => match root.as_ref() {
            Expression::Program(items) => items.clone(),
            _ => Vec::new(),
        },
        Err(_) => Vec::new(),
    };
    let mut last_use_end = None;
    for (span, item) in &items {
        let Expression::Use { path: use_path, .. } = item.as_ref() else {
            continue;
        };
        // The node span can run past the `;`; the statement ends there.
        let end = text[span.start..]
            .find(';')
            .map_or(span.end, |i| span.start + i + 1);
        last_use_end = Some(end);
        let statement = &text[span.start..end];
        if use_path.iter().map(String::as_str).eq(path.iter().copied())
            && let (Some(_), Some(close)) = (statement.find('{'), statement.rfind('}'))
        {
            let at = span.start + close;
            let before = text[..at].trim_end();
            let sep = if before.ends_with(',') { " " } else { ", " };
            return TextEdit {
                range: byte_range(text, &(before.len()..before.len())),
                new_text: format!("{sep}{name}"),
            };
        }
    }
    match last_use_end {
        Some(end) => {
            let line_end = text[end..].find('\n').map_or(text.len(), |i| end + i);
            TextEdit {
                range: byte_range(text, &(line_end..line_end)),
                new_text: format!("\nuse {module}::{{{name}}};"),
            }
        }
        None => TextEdit {
            range: byte_range(text, &(0..0)),
            new_text: format!("use {module}::{{{name}}};\n\n"),
        },
    }
}

/// Parameter names of the first `fn name` declared in `source`.
fn function_parameter_list(source: &str, name: &str) -> Option<Vec<String>> {
    let ast = Pratt::default().parse(source).ok()?;
    let mut found = None;
    visit_nodes(&ast, &mut |node| {
        if found.is_none()
            && let Expression::Function { name: fn_name, args, .. } = node.1.as_ref()
            && *fn_name == name
        {
            found = Some(function_parameter_names(args));
        }
    });
    found
}

/// A receiver member for completion: name, type text, is a method.
type MemberInfo = (String, String, bool);

/// Class of `segments` whose `.member` sits at `access` in `uri`'s buffer,
/// with its members. Typechecks a copy with the access removed (`p.su` →
/// `p;`) so a half-typed member still types its receiver, then restores
/// the project.
fn receiver_class(
    state: &mut ServerState,
    uri: &Uri,
    segments: &[String],
    access: Range<usize>,
) -> Option<(String, Vec<MemberInfo>)> {
    let text = state.documents.get(uri)?.text.clone();
    let path = uri_path(uri)?;
    // The receiver's first identifier starts `segments.join(".").len()` before the dot.
    let chain_len = segments.join(".").len();
    let base_start = access.start.checked_sub(chain_len)?;
    let base_range = base_start..base_start + segments[0].len();
    // Blank the access when the statement goes on (`self.x;` → `self  ;`),
    // else end it (`p.su⏎}` → `p;⏎}`).
    let rest = &text[access.end..];
    let continues = rest.trim_start().starts_with([';', ')', ',', ']']);
    let filler = if continues {
        " ".repeat(access.len())
    } else {
        ";".to_string()
    };
    let repaired = format!("{}{filler}{rest}", &text[..access.start]);
    let ast = Pratt::default().parse(&repaired).ok()?;
    let index = state.project_index.as_mut()?;
    index.apply_open_file(path.clone(), repaired.clone());
    let _ = index.typecheck_entry(&path);
    let checker = index.checker();
    let found = (|| {
        // `self` has no typed node: it is the enclosing `impl`'s owner.
        let mut ty = if segments[0] == "self" {
            let mut owner = None;
            visit_nodes(&ast, &mut |node| {
                if let Expression::Implementation { owner: o, .. } = node.1.as_ref()
                    && (node.0.start..node.0.end).contains(&base_range.start)
                {
                    owner = Some(o.to_string());
                }
            });
            owner?
        } else {
            hover_type(
                &ast,
                Some(checker),
                &repaired,
                &segments[0],
                base_range.start,
                &base_range,
            )?
        };
        for field in &segments[1..] {
            let members = members_of(checker, &class_of_type_text(&ty));
            let (_, field_ty, _, _) = members.into_iter().find(|m| &m.0 == field && !m.2)?;
            ty = format_ty_for_diag(checker.subst(), &field_ty);
        }
        let class = class_of_type_text(&ty);
        let members: Vec<MemberInfo> = members_of(checker, &class)
            .into_iter()
            .map(|(name, ty, is_method, _)| {
                (name, format_ty_for_diag(checker.subst(), &ty), is_method)
            })
            .collect();
        (!members.is_empty()).then_some((class, members))
    })();
    // Back to the real buffer (and its diagnostics / span tables).
    refresh_project(state, &path);
    found
}

/// Members of `class`, trying the bare name when the key is qualified.
fn members_of(checker: &Checker, class: &str) -> Vec<(String, compiler::Ty, bool, bool)> {
    let members = checker.class_members(class);
    if !members.is_empty() {
        return members;
    }
    class
        .rsplit("::")
        .next()
        .map(|bare| checker.class_members(bare))
        .unwrap_or_default()
}

/// Completion items for `recv.pre`: the receiver class's fields and methods.
fn member_completions(
    state: &mut ServerState,
    uri: &Uri,
    position: Position,
) -> Option<Vec<CompletionItem>> {
    let text = state.documents.get(uri)?.text.clone();
    let offset = position_to_byte(&text, position)?;
    let (segments, prefix, access) = member_access_at(&text, offset)?;
    let (_, members) = receiver_class(state, uri, &segments, access)?;
    let items = members
        .into_iter()
        .filter(|(name, ..)| name.starts_with(&prefix))
        .map(|(name, detail, is_method)| CompletionItem {
            label: name.clone(),
            kind: Some(if is_method {
                CompletionItemKind::METHOD
            } else {
                CompletionItemKind::FIELD
            }),
            detail: Some(detail),
            insert_text: Some(if is_method { format!("{name}($0)") } else { name }),
            insert_text_format: is_method.then_some(InsertTextFormat::SNIPPET),
            ..CompletionItem::default()
        })
        .collect();
    Some(items)
}

/// Go to the declaration of the member under the cursor (`p.sum`, `p.x`).
fn member_definition(state: &mut ServerState, uri: &Uri, position: Position) -> Option<Location> {
    let text = state.documents.get(uri)?.text.clone();
    let offset = position_to_byte(&text, position)?;
    let word = word_range(&text, offset)?;
    let member = text[word.clone()].to_string();
    let (segments, _, access) = member_access_at(&text, word.end)?;
    let (class, _) = receiver_class(state, uri, &segments, access)?;
    let bare = class.rsplit("::").next().unwrap_or(&class).to_string();
    // Search open buffers, then indexed project files, for the declaration.
    let mut sources: Vec<(Uri, String)> = state
        .documents
        .iter()
        .map(|(u, d)| (u.clone(), d.text.clone()))
        .collect();
    if let Some(index) = &state.project_index {
        for path in index.indexed_paths() {
            if let (Some(u), Some(src)) = (path_to_uri(path), index.source_for(path))
                && !sources.iter().any(|(existing, _)| existing == &u)
            {
                sources.push((u, src.to_string()));
            }
        }
    }
    for (u, src) in sources {
        if let Some(range) = member_decl_range(&src, &bare, &member) {
            return Some(Location {
                uri: u,
                range: byte_range(&src, &range),
            });
        }
    }
    None
}

/// Name span of field `member` in `class Owner` or method `member` in an
/// `impl Owner` block of `source`.
fn member_decl_range(source: &str, owner: &str, member: &str) -> Option<Range<usize>> {
    let ast = Pratt::default().parse(source).ok()?;
    let offset_of = |name: &str| {
        let base = source.as_ptr() as usize;
        let ptr = name.as_ptr() as usize;
        (ptr >= base && ptr + name.len() <= base + source.len()).then(|| ptr - base)
    };
    let mut found = None;
    visit_nodes(&ast, &mut |node| {
        if found.is_some() {
            return;
        }
        match node.1.as_ref() {
            Expression::Class { name, fields, .. } if *name == owner => {
                for (_, field) in fields {
                    if let Expression::Field { name: fname, .. } = field.as_ref()
                        && let Expression::Identifier(f) = fname.1.as_ref()
                        && *f == member
                    {
                        found = offset_of(f).map(|o| o..o + f.len());
                    }
                }
            }
            Expression::Implementation { owner: o, methods, .. } if *o == owner => {
                for method in methods {
                    let mut m = method;
                    if let Expression::Method(_, inner) = m.1.as_ref() {
                        m = inner;
                    }
                    if let Expression::Function { name, .. } = m.1.as_ref()
                        && *name == member
                    {
                        found = offset_of(name).map(|o| o..o + name.len());
                    }
                }
            }
            _ => {}
        }
    });
    found
}

fn hover(document: &Document, position: Position) -> Option<Hover> {
    let offset = position_to_byte(&document.text, position)?;
    let range = word_range(&document.text, offset)?;
    let name = document.text.get(range.clone())?.to_owned();

    if let Some(hover) = hover_from_source(&document.text, &name, offset, range.clone())
        && hover_has_detail(&hover) {
            return Some(hover);
        }

    // Mid-edit buffers often fail to parse because of an incomplete *other*
    // token (e.g. `fi` in `main` while hovering `fib`). Repair near EOF, not
    // under the hover word (which would delete the declaration being inspected).
    let repair_offset = document.text.len().saturating_sub(1);
    if repair_offset < range.start || repair_offset >= range.end {
        for sanitized in sanitize_variants(&document.text, repair_offset) {
            if let Some(hover) = hover_from_source(&sanitized, &name, offset, range.clone())
                && hover_has_detail(&hover) {
                    return Some(hover);
                }
        }
    }

    let mut ty_text = None;
    let mut docs = None;
    if let Some(candidate) = document
        .last_good
        .as_ref()
        .and_then(|good| good.candidates.get(&name))
    {
        ty_text = candidate.detail.clone();
        docs = candidate.documentation.clone();
    }

    hover_markup(&document.text, &name, range, ty_text, docs)
}

fn hover_has_detail(hover: &Hover) -> bool {
    match &hover.contents {
        HoverContents::Markup(MarkupContent { value, .. }) => {
            value.contains("```")
                || value.contains("\n\n---\n\n")
                || value.contains("docs/references/")
                || value.contains("src/content/docs/")
        }
        _ => false,
    }
}

fn hover_from_source(
    source: &str,
    name: &str,
    offset: usize,
    range: Range<usize>,
) -> Option<Hover> {
    let ast = Pratt::default().parse(source).ok()?;
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);

    // Name bindings are the most reliable type source for identifier hovers;
    // expression-span tables often record the enclosing statement's type.
    let mut ty = checker.env().lookup(name).map(|scheme| scheme.ty.clone());
    if ty.is_none() {
        for span in spans_containing(&ast, offset) {
            if let Some(found) = checker.lookup_for_codegen_span(span.start, span.end) {
                ty = Some(found);
                break;
            }
        }
    }
    if ty.is_none() {
        ty = checker.lookup_for_codegen_span(range.start, range.end);
    }

    let ty_text = find_parameter_type_for_name(ast.1.as_ref(), name).or_else(|| {
        ty.as_ref()
            .map(|ty| format_ty_for_diag(checker.subst(), ty))
    });
    let docs = find_param_docs_for_name(ast.1.as_ref(), name)
        .or_else(|| find_docs_for_name(ast.1.as_ref(), name))
        .or_else(|| {
            virtual_completion_candidates(ast.1.as_ref())
                .remove(name)
                .map(|(_, docs)| docs)
        });
    let docs = match (fn_effects(&checker, &ast, name), docs) {
        (Some(fx), Some(docs)) => Some(format!("{fx}\n\n{docs}")),
        (fx, docs) => fx.or(docs),
    };
    hover_markup(source, name, range, ty_text, docs)
}

/// `**effects:** …` for a function or method named `name` in this file.
fn fn_effects(checker: &Checker, ast: &Output<'_>, name: &str) -> Option<String> {
    // HIR is built from a checked program only.
    if checker.messages().iter().any(|m| *m.kind() == MessageKind::ERROR) {
        return None;
    }
    let suffix = format!("::{name}");
    let found: Vec<String> = compiler::describe_fns(checker, ast)
        .into_iter()
        .filter(|(n, _)| n == name || n.ends_with(&suffix))
        .map(|(_, fx)| fx)
        .collect();
    match found.as_slice() {
        [fx] => Some(format!("**effects:** {fx}")),
        _ => None,
    }
}

fn hover_markup(
    source: &str,
    name: &str,
    range: Range<usize>,
    ty_text: Option<String>,
    docs: Option<String>,
) -> Option<Hover> {
    if ty_text.is_none() && docs.is_none() {
        // Still show the bare name so K is never a dead end on a known identifier.
        if name.is_empty() {
            return None;
        }
        return Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!("`{name}`"),
            }),
            range: Some(byte_range(source, &range)),
        });
    }

    let mut value = String::new();
    if let Some(ty_text) = ty_text {
        value.push_str("```coil\n");
        value.push_str(&format!("{name}: {ty_text}"));
        value.push_str("\n```");
    } else {
        value.push_str(&format!("`{name}`"));
    }
    if let Some(docs) = docs {
        if !value.is_empty() {
            value.push_str("\n\n---\n\n");
        }
        value.push_str(&docs);
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(byte_range(source, &range)),
    })
}

/// Collect AST spans that contain `offset`, innermost first.
fn spans_containing(ast: &Output<'_>, offset: usize) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    collect_spans_containing(ast, offset, &mut spans);
    spans.sort_by_key(|span| span.end - span.start);
    spans
}

fn collect_spans_containing(node: &Output<'_>, offset: usize, out: &mut Vec<Range<usize>>) {
    let span = node.0.start..node.0.end;
    if offset < span.start || offset > span.end {
        return;
    }
    out.push(span);
    walk_children(node.1.as_ref(), offset, out);
}

fn walk_children(expression: &Expression<'_>, offset: usize, out: &mut Vec<Range<usize>>) {
    match expression {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            for item in items {
                collect_spans_containing(item, offset, out);
            }
        }
        Expression::Function { args, body, .. } => {
            collect_spans_containing(args, offset, out);
            if let Some(body) = body {
                collect_spans_containing(body, offset, out);
            }
        }
        Expression::Call { name, args } => {
            collect_spans_containing(name, offset, out);
            if let Some(args) = args {
                for arg in args {
                    collect_spans_containing(arg, offset, out);
                }
            }
        }
        Expression::Return(inner)
        | Expression::ImplicitReturn(inner)
        | Expression::Expr(inner)
        | Expression::Group(inner)
        | Expression::ExprStatement(inner)
        | Expression::Statement(inner)
        | Expression::Method(_, inner) => collect_spans_containing(inner, offset, out),
        Expression::Add(a, b)
        | Expression::Sub(a, b)
        | Expression::Mul(a, b)
        | Expression::Div(a, b)
        | Expression::Assignment(a, b)
        | Expression::Eq(a, b)
        | Expression::Neq(a, b)
        | Expression::Le(a, b)
        | Expression::Gt(a, b)
        | Expression::Leq(a, b)
        | Expression::Geq(a, b) => {
            collect_spans_containing(a, offset, out);
            collect_spans_containing(b, offset, out);
        }
        Expression::If(branches) => {
            for branch in branches {
                collect_spans_containing(branch, offset, out);
            }
        }
        Expression::Variable(_, Some(init)) | Expression::Constant(_, Some(init)) => {
            collect_spans_containing(init, offset, out);
        }
        Expression::Class { fields, .. } => {
            for field in fields {
                collect_spans_containing(field, offset, out);
            }
        }
        Expression::Implementation { methods, .. } => {
            for method in methods {
                collect_spans_containing(method, offset, out);
            }
        }
        Expression::EnumDecl { variants, .. } => {
            for variant in variants {
                collect_spans_containing(variant, offset, out);
            }
        }
        Expression::Match { scrutinee, arms } => {
            collect_spans_containing(scrutinee, offset, out);
            for arm in arms {
                collect_spans_containing(&arm.body, offset, out);
            }
        }
        Expression::IfLet {
            scrutinee,
            then_arm,
            else_arm,
        } => {
            collect_spans_containing(scrutinee, offset, out);
            collect_spans_containing(&then_arm.body, offset, out);
            collect_spans_containing(&else_arm.body, offset, out);
        }
        Expression::WhileLet {
            scrutinee,
            then_arm,
            on_miss,
        } => {
            collect_spans_containing(scrutinee, offset, out);
            collect_spans_containing(&then_arm.body, offset, out);
            collect_spans_containing(&on_miss.body, offset, out);
        }
        _ => {}
    }
}

fn find_docs_for_name(expression: &Expression<'_>, name: &str) -> Option<String> {
    match expression {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            items
                .iter()
                .find_map(|(_, item)| find_docs_for_name(item, name))
        }
        Expression::Function {
            name: item_name,
            docs,
            args,
            body,
            ..
        } => {
            if *item_name == name {
                let mut text = docs_markdown(docs).unwrap_or_default();
                if let Some(parameters) = parameter_docs_markdown(args) {
                    text.push_str(&parameters);
                }
                return (!text.is_empty()).then_some(text);
            }
            body.as_ref()
                .and_then(|body| find_docs_for_name(body.1.as_ref(), name))
        }
        Expression::Class {
            name: item_name,
            docs,
            fields,
            ..
        } => {
            if *item_name == name {
                return docs_markdown(docs);
            }
            fields
                .iter()
                .find_map(|(_, field)| find_docs_for_name(field, name))
        }
        Expression::EnumDecl {
            name: item_name,
            docs,
            ..
        }
        | Expression::TypeAlias {
            name: item_name,
            docs,
            ..
        }
        | Expression::AttrDecl {
            name: item_name,
            docs,
            ..
        }
        | Expression::DeriveDecl {
            name: item_name,
            docs,
            ..
        }
        | Expression::FnMacroDecl {
            name: item_name,
            docs,
            ..
        } if *item_name == name => docs_markdown(docs),
        Expression::Method(_, inner) => find_docs_for_name(inner.1.as_ref(), name),
        Expression::Implementation { methods, .. } => methods
            .iter()
            .find_map(|(_, method)| find_docs_for_name(method, name)),
        _ => None,
    }
}

fn find_param_docs_for_name(expression: &Expression<'_>, name: &str) -> Option<String> {
    match expression {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            items
                .iter()
                .find_map(|(_, item)| find_param_docs_for_name(item, name))
        }
        Expression::Function { args, body, .. } => {
            if let Expression::Fragment(items) = args.1.as_ref()
                && let Some(docs) = items.iter().find_map(|(_, item)| {
                    let Expression::Argument {
                        docs,
                        name: param_name,
                        ..
                    } = item.as_ref()
                    else {
                        return None;
                    };
                    (*param_name == name && !docs.is_empty())
                        .then(|| docs_markdown(docs))
                }) {
                    return docs;
                }
            body.as_ref()
                .and_then(|body| find_param_docs_for_name(body.1.as_ref(), name))
        }
        Expression::Method(_, inner) => find_param_docs_for_name(inner.1.as_ref(), name),
        Expression::Implementation { methods, .. } => methods
            .iter()
            .find_map(|(_, method)| find_param_docs_for_name(method, name)),
        _ => None,
    }
}

/// Innermost unclosed `(` before the end of `prefix` (skipping strings and
/// `//` comments) and the number of top-level commas after it.
fn enclosing_call_paren(prefix: &str) -> Option<(usize, u32)> {
    let mut stack: Vec<(usize, u32)> = Vec::new();
    let bytes = prefix.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'(' | b'[' | b'{' => stack.push((i, 0)),
            b')' | b']' | b'}' => {
                stack.pop();
            }
            b',' => {
                if let Some(top) = stack.last_mut() {
                    top.1 += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let &(open, commas) = stack.last()?;
    (bytes[open] == b'(').then_some((open, commas))
}

/// Signature of the call around the cursor, read from the callee's
/// declaration (possibly in another file).
fn decl_signature_help(state: &ServerState, uri: &Uri, position: Position) -> Option<SignatureHelp> {
    let text = &state.documents.get(uri)?.text;
    let offset = position_to_byte(text, position)?;
    let (open, commas) = enclosing_call_paren(&text[..offset])?;
    let (name_start, name) = callee_name_before_paren(text, open)?;
    let name_position = byte_position(text, name_start);
    let mut sources: Vec<String> = goto_definitions(state, uri, name_position)
        .into_iter()
        .filter_map(|location| {
            let path = uri_path(&location.uri)?;
            state
                .documents
                .get(&location.uri)
                .map(|d| d.text.clone())
                .or_else(|| {
                    state
                        .project_index
                        .as_ref()
                        .and_then(|index| index.source_for(&path).map(str::to_owned))
                })
                .or_else(|| std::fs::read_to_string(&path).ok())
        })
        .collect();
    // Methods have no goto target yet: fall back to this file and the
    // indexed project files. The call being typed rarely parses; blank its
    // lines (same length, so declaration spans stay valid).
    sources.push(text.clone());
    let line_start = text[..open].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[offset..].find('\n').map_or(text.len(), |i| offset + i);
    let mut repaired = text.clone().into_bytes();
    for byte in &mut repaired[line_start..line_end] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
    sources.push(String::from_utf8(repaired).unwrap_or_default());
    if let Some(index) = &state.project_index {
        for path in index.indexed_paths() {
            if let Some(source) = index.source_for(path) {
                sources.push(source.to_owned());
            }
        }
    }
    let signature = sources
        .iter()
        .find_map(|source| function_signature(source, name))?;
    let active = commas.min(signature.parameters.len().saturating_sub(1) as u32);
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label: signature.label,
            documentation: signature.docs.map(|docs| {
                Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: docs,
                })
            }),
            parameters: Some(signature.parameters),
            active_parameter: Some(active),
        }],
        active_signature: Some(0),
        active_parameter: Some(active),
    })
}

struct FnSignature {
    label: String,
    docs: Option<String>,
    parameters: Vec<ParameterInformation>,
}

/// `name(T a, U b) -> R` for the first `fn` / `macro` / `attr` / `derive` of `name`.
fn function_signature(source: &str, name: &str) -> Option<FnSignature> {
    let ast = Pratt::default().parse(source).ok()?;
    let mut found = None;
    visit_nodes(&ast, &mut |node| {
        if found.is_some() {
            return;
        }
        let (fn_name, docs, args, returns) = match node.1.as_ref() {
            Expression::Function {
                name: fn_name,
                docs,
                args,
                returns,
                ..
            }
            | Expression::AttrDecl {
                name: fn_name,
                docs,
                args,
                returns,
                ..
            }
            | Expression::DeriveDecl {
                name: fn_name,
                docs,
                args,
                returns,
                ..
            }
            | Expression::FnMacroDecl {
                name: fn_name,
                docs,
                args,
                returns,
                ..
            } => (fn_name, docs, args, returns),
            _ => return,
        };
        if *fn_name != name {
            return;
        }
        found = Some(signature_from_params(source, name, docs, args, returns.as_ref()));
    });
    found
}

fn signature_from_params(
    source: &str,
    name: &str,
    docs: &[&str],
    args: &Output<'_>,
    returns: Option<&Output<'_>>,
) -> FnSignature {
    let mut parameters = Vec::new();
    if let Expression::Fragment(items) = args.1.as_ref() {
        for (span, item) in items {
            let Expression::Argument { docs, .. } = item.as_ref() else {
                continue;
            };
            let label = source[span.start..span.end]
                .lines()
                .filter(|line| !line.trim_start().starts_with("///"))
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" ");
            parameters.push(ParameterInformation {
                label: lsp_types::ParameterLabel::Simple(label),
                documentation: docs_markdown(docs).map(|value| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value,
                    })
                }),
            });
        }
    }
    let params_text = parameters
        .iter()
        .map(|p| match &p.label {
            lsp_types::ParameterLabel::Simple(label) => label.clone(),
            lsp_types::ParameterLabel::LabelOffsets(_) => String::new(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let ret = returns
        .map(|r| format!(" -> {}", source[r.0.start..r.0.end].trim()))
        .unwrap_or_default();
    FnSignature {
        label: format!("{name}({params_text}){ret}"),
        docs: docs_markdown(docs),
        parameters,
    }
}

/// Identifier immediately before `(`; `name!(` strips the `!`.
fn callee_name_before_paren(prefix: &str, open: usize) -> Option<(usize, &str)> {
    let mut name_end = prefix[..open].trim_end().len();
    if name_end > 0 && prefix.as_bytes()[name_end - 1] == b'!' {
        name_end = prefix[..name_end - 1].trim_end().len();
    }
    let name_start = prefix[..name_end]
        .rfind(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .map(|index| index + 1)
        .unwrap_or(0);
    let name = &prefix[name_start..name_end];
    (!name.is_empty()).then_some((name_start, name))
}

fn signature_help(document: &Document, position: Position) -> Option<SignatureHelp> {
    let source = &document.text;
    let offset = position_to_byte(source, position)?;
    let prefix = &source[..offset.min(source.len())];
    let open = prefix.rfind('(')?;
    let (_, name) = callee_name_before_paren(prefix, open)?;
    let declaration = ["fn ", "macro ", "attr ", "derive "]
        .iter()
        .find_map(|prefix_kw| source.find(&format!("{prefix_kw}{name}")));
    let Some(declaration) = declaration else {
        return signature_help_from_index(document, name, prefix, open);
    };
    let params_start = source[declaration..].find('(')? + declaration + 1;
    let params_end = source[params_start..].find(')')? + params_start;
    let mut parameters = source[params_start..params_end]
        .split(',')
        .map(str::trim)
        .filter(|parameter| !parameter.is_empty())
        .map(|parameter| ParameterInformation {
            label: lsp_types::ParameterLabel::Simple(parameter.to_owned()),
            documentation: None,
        })
        .collect::<Vec<_>>();
    if let Ok(ast) = Pratt::default().parse(source)
        && let Some(docs) = function_parameter_docs(ast.1.as_ref(), name) {
            for (parameter, docs) in parameters.iter_mut().zip(docs) {
                if let Some(docs) = docs {
                    parameter.documentation = Some(Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: docs,
                    }));
                }
            }
        }
    let active_parameter = prefix[open + 1..]
        .matches(',')
        .count()
        .min(parameters.len().saturating_sub(1)) as u32;
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label: format!(
                "{name}({})",
                parameters
                    .iter()
                    .map(|p| match &p.label {
                        lsp_types::ParameterLabel::Simple(label) => label.clone(),
                        lsp_types::ParameterLabel::LabelOffsets(_) => String::new(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            documentation: None,
            parameters: Some(parameters),
            active_parameter: Some(active_parameter),
        }],
        active_signature: Some(0),
        active_parameter: Some(active_parameter),
    })
}

fn signature_help_from_index(
    document: &Document,
    name: &str,
    prefix: &str,
    open: usize,
) -> Option<SignatureHelp> {
    let semantic = analyze_for_completions_at(&document.text, Some(document.text.len().saturating_sub(1)))
        .or_else(|| analyze_for_completions(&format!("{});", document.text)))
        .or_else(|| document.last_good.clone())
        .or_else(|| virtual_signature_candidate(name))?;
    let candidate = semantic.candidates.get(name)?;
    let parameters: Vec<ParameterInformation> = if candidate.parameter_names.is_empty() {
        candidate
            .detail
            .as_deref()
            .map(|detail| {
                vec![ParameterInformation {
                    label: lsp_types::ParameterLabel::Simple(detail.to_owned()),
                    documentation: candidate.documentation.as_ref().map(|docs| {
                        Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: docs.clone(),
                        })
                    }),
                }]
            })
            .unwrap_or_default()
    } else {
        candidate
            .parameter_names
            .iter()
            .map(|parameter| ParameterInformation {
                label: lsp_types::ParameterLabel::Simple(parameter.clone()),
                documentation: None,
            })
            .collect()
    };
    let active_parameter = prefix[open + 1..]
        .matches(',')
        .count()
        .min(parameters.len().saturating_sub(1)) as u32;
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label: candidate
                .detail
                .clone()
                .unwrap_or_else(|| format!("{name}(...)")),
            documentation: candidate.documentation.as_ref().map(|docs| {
                Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: docs.clone(),
                })
            }),
            parameters: Some(parameters),
            active_parameter: Some(active_parameter),
        }],
        active_signature: Some(0),
        active_parameter: Some(active_parameter),
    })
}

fn virtual_signature_candidate(name: &str) -> Option<GoodAnalysis> {
    let modules = VirtualModules::new();
    for module in [
        "prelude",
        "prelude::ops",
        "prelude::test",
        "prelude::math",
        "io",
        "io::fs",
        "string",
        "thread",
        "env",
        "gc",
        "ffi",
        "clock",
    ] {
        let path = module
            .split("::")
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if let Some(export) = modules.resolve_item(&path, name) {
            let mut candidates = HashMap::new();
            candidates.insert(
                name.to_owned(),
                CompletionCandidate {
                    label: name.to_owned(),
                    kind: virtual_completion_kind(&export),
                    detail: export
                        .host_registry()
                        .map(|reg| format!("HostInvoke `{reg}`"))
                        .or_else(|| Some(format!("{module}::{name}"))),
                    documentation: Some(builtin_documentation(&path, name, &export)),
                    parameter_names: Vec::new(),
                },
            );
            return Some(GoodAnalysis { candidates });
        }
    }
    None
}

fn find_parameter_type_for_name(expression: &Expression<'_>, name: &str) -> Option<String> {
    match expression {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            items
                .iter()
                .find_map(|(_, item)| find_parameter_type_for_name(item, name))
        }
        Expression::Function { args, body, .. } => {
            if let Expression::Fragment(items) = args.1.as_ref()
                && let Some(ty) = items.iter().find_map(|(_, item)| {
                    let Expression::Argument {
                        ty,
                        name: parameter_name,
                        ..
                    } = item.as_ref()
                    else {
                        return None;
                    };
                    (*parameter_name == name)
                        .then(|| ty.as_ref().map(|ty| ty.1.to_string()))
                        .flatten()
                }) {
                    return Some(ty);
                }
            body.as_ref()
                .and_then(|body| find_parameter_type_for_name(body.1.as_ref(), name))
        }
        Expression::Method(_, inner) => find_parameter_type_for_name(inner.1.as_ref(), name),
        Expression::Implementation { methods, .. } => methods
            .iter()
            .find_map(|(_, method)| find_parameter_type_for_name(method, name)),
        _ => None,
    }
}

fn function_parameter_docs(
    expression: &Expression<'_>,
    function_name: &str,
) -> Option<Vec<Option<String>>> {
    match expression {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            items
                .iter()
                .find_map(|(_, item)| function_parameter_docs(item, function_name))
        }
        Expression::Function { name, args, body, .. } => {
            if *name == function_name {
                let Expression::Fragment(items) = args.1.as_ref() else {
                    return Some(Vec::new());
                };
                return Some(
                    items
                        .iter()
                        .filter_map(|(_, item)| {
                            let Expression::Argument { docs, .. } = item.as_ref() else {
                                return None;
                            };
                            Some(docs_markdown(docs))
                        })
                        .collect(),
                );
            }
            body.as_ref()
                .and_then(|body| function_parameter_docs(body.1.as_ref(), function_name))
        }
        Expression::Method(_, inner) => function_parameter_docs(inner.1.as_ref(), function_name),
        Expression::Implementation { methods, .. } => methods
            .iter()
            .find_map(|(_, method)| function_parameter_docs(method, function_name)),
        _ => None,
    }
}

fn full_range(source: &str) -> LspRange {
    byte_range(source, &(0..source.len()))
}

fn byte_range(source: &str, range: &Range<usize>) -> LspRange {
    LspRange {
        start: byte_position(source, range.start),
        end: byte_position(source, range.end),
    }
}

fn byte_position(source: &str, byte: usize) -> Position {
    let byte = byte.min(source.len());
    let mut line = 0;
    let mut line_start = 0;
    for (index, character) in source.char_indices() {
        if index >= byte {
            break;
        }
        if character == '\n' {
            line += 1;
            line_start = index + character.len_utf8();
        }
    }
    let character = source[line_start..byte]
        .chars()
        .map(|character| character.len_utf16() as u32)
        .sum();
    Position { line, character }
}

fn position_to_byte(source: &str, position: Position) -> Option<usize> {
    let line_start = if position.line == 0 {
        0
    } else {
        source
            .match_indices('\n')
            .nth(position.line as usize - 1)
            .map(|(index, _)| index + 1)
            .unwrap_or(source.len())
    };
    let line = &source[line_start..];
    let mut utf16 = 0;
    for (offset, character) in line.char_indices() {
        if utf16 >= position.character {
            return Some(line_start + offset);
        }
        utf16 += character.len_utf16() as u32;
    }
    Some(source.len())
}

fn word_range(source: &str, offset: usize) -> Option<Range<usize>> {
    let bytes = source.as_bytes();
    let mut start = offset.min(bytes.len());
    let mut end = start;
    while start > 0 && is_ident(bytes[start - 1]) {
        start -= 1;
    }
    while end < bytes.len() && is_ident(bytes[end]) {
        end += 1;
    }
    (start < end).then_some(start..end)
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[cfg(test)]
fn occurrences(source: &str, word: &str) -> Vec<Range<usize>> {
    source
        .match_indices(word)
        .filter_map(|(start, _)| {
            let end = start + word.len();
            let before = start
                .checked_sub(1)
                .and_then(|index| source.as_bytes().get(index));
            let after = source.as_bytes().get(end);
            (before.is_none_or(|byte| !is_ident(*byte))
                && after.is_none_or(|byte| !is_ident(*byte)))
            .then_some(start..end)
        })
        .collect()
}

fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !coil_keywords().contains(&name)
        && name != "self"
}

fn coil_keywords() -> &'static [&'static str] {
    &[
        "fn", "let", "const", "class", "enum", "type", "if", "else", "for", "while", "in",
        "match", "return", "true", "false", "use", "mod", "pub", "static", "async", "defer",
        "raise", "panic", "yield", "break", "continue", "where", "impl", "trait", "extern", "as",
        "readonly", "new", "default", "typeof", "resume", "with", "done", "attr", "struct",
        "test", "forall", "from",
    ]
}

/// Contextual words that highlight as keywords (`macro name(`, `quote items`).
fn highlight_keywords() -> &'static [&'static str] {
    &["derive", "macro", "quote", "attrs"]
}

const TOKEN_KEYWORD: u32 = 0;
const TOKEN_FUNCTION: u32 = 1;
const TOKEN_TYPE: u32 = 2;
const TOKEN_VARIABLE: u32 = 3;
const TOKEN_COMMENT: u32 = 4;
const TOKEN_STRING: u32 = 5;
const TOKEN_NUMBER: u32 = 6;
const TOKEN_NAMESPACE: u32 = 7;
const TOKEN_OPERATOR: u32 = 8;
const TOKEN_MACRO: u32 = 9;
const TOKEN_TYPED_PRIORITY: u8 = 5;
const TOKEN_AST_PRIORITY: u8 = 4;

#[derive(Clone)]
struct SpannedToken {
    range: Range<usize>,
    token_type: u32,
    priority: u8,
}

fn lsp_range_to_byte_range(source: &str, range: LspRange) -> Option<Range<usize>> {
    let start = position_to_byte(source, range.start)?;
    let end = position_to_byte(source, range.end)?;
    Some(start..end.max(start))
}

fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

fn range_intersects(filter: &Range<usize>, token: &Range<usize>) -> bool {
    ranges_overlap(filter, token)
}

fn merge_spanned_tokens(mut tokens: Vec<SpannedToken>) -> Vec<SpannedToken> {
    tokens.sort_by(|left, right| {
        left.range
            .start
            .cmp(&right.range.start)
            .then(right.priority.cmp(&left.priority))
            .then(right.range.end.cmp(&left.range.end))
    });
    let mut accepted: Vec<SpannedToken> = Vec::new();
    'next: for token in tokens {
        for kept in &accepted {
            if ranges_overlap(&kept.range, &token.range) {
                continue 'next;
            }
        }
        accepted.push(token);
    }
    accepted.sort_by_key(|token| token.range.start);
    accepted
}

fn symbol_kind_to_token_type(kind: SymbolKind) -> u32 {
    match kind {
        SymbolKind::Function | SymbolKind::Method => TOKEN_FUNCTION,
        SymbolKind::Macro => TOKEN_MACRO,
        SymbolKind::Class | SymbolKind::Enum | SymbolKind::TypeAlias => TOKEN_TYPE,
        SymbolKind::Namespace => TOKEN_NAMESPACE,
        SymbolKind::Variable => TOKEN_VARIABLE,
    }
}

fn reference_token_type(index: &SymbolIndex, name: &str) -> u32 {
    index
        .definitions(name)
        .first()
        .map(|definition| symbol_kind_to_token_type(definition.kind))
        .unwrap_or(TOKEN_VARIABLE)
}

fn definition_token_type(checker: &Checker, definition: &compiler::SymbolDef) -> u32 {
    match definition.kind {
        SymbolKind::Class | SymbolKind::Enum | SymbolKind::TypeAlias | SymbolKind::Namespace => {
            symbol_kind_to_token_type(definition.kind)
        }
        SymbolKind::Function | SymbolKind::Method => TOKEN_FUNCTION,
        SymbolKind::Macro => TOKEN_MACRO,
        SymbolKind::Variable => checker
            .codegen_var_type(&definition.name)
            .map(semantic_token_type_for_ty)
            .or_else(|| {
                checker
                    .env()
                    .lookup(&definition.name)
                    .map(|scheme| semantic_token_type_for_ty(&scheme.ty))
            })
            .unwrap_or(TOKEN_VARIABLE),
    }
}

fn type_for_reference_site(
    checker: &Checker,
    index: &SymbolIndex,
    site: &compiler::RefSite,
) -> Option<u32> {
    if index.definitions(&site.name).first().is_some_and(|definition| {
        matches!(
            definition.kind,
            SymbolKind::Namespace | SymbolKind::Class | SymbolKind::Enum | SymbolKind::TypeAlias
        )
    }) {
        return Some(reference_token_type(index, &site.name));
    }
    if let Some(ty) = checker.lookup_for_codegen_span(site.range.start, site.range.end) {
        return Some(semantic_token_type_for_ty(&ty));
    }
    if let Some(ty) = checker.codegen_var_type(&site.name) {
        return Some(semantic_token_type_for_ty(ty));
    }
    if let Some(scheme) = checker.env().lookup(&site.name) {
        return Some(semantic_token_type_for_ty(&scheme.ty));
    }
    None
}

fn reference_token_type_typeaware(
    checker: &Checker,
    index: &SymbolIndex,
    site: &compiler::RefSite,
) -> u32 {
    type_for_reference_site(checker, index, site)
        .unwrap_or_else(|| reference_token_type(index, &site.name))
}

fn typed_semantic_tokens(source: &str, file: &PathBuf) -> Option<Vec<SpannedToken>> {
    let ast = Pratt::default().parse(source).ok()?;
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let index = SymbolIndex::from_source(file.clone(), source);
    let mut tokens = Vec::new();
    for definition in index.all_definitions() {
        if definition.file != *file {
            continue;
        }
        tokens.push(SpannedToken {
            range: definition.name_range.clone(),
            token_type: definition_token_type(&checker, definition),
            priority: TOKEN_TYPED_PRIORITY,
        });
    }
    for site in index.all_reference_sites() {
        if site.file != *file {
            continue;
        }
        tokens.push(SpannedToken {
            range: site.range.clone(),
            token_type: reference_token_type_typeaware(&checker, &index, site),
            priority: TOKEN_TYPED_PRIORITY,
        });
    }
    Some(tokens)
}

fn ast_semantic_tokens(source: &str, file: &PathBuf) -> Vec<SpannedToken> {
    let index = SymbolIndex::from_source(file.clone(), source);
    let mut tokens = Vec::new();
    for definition in index.all_definitions() {
        if definition.file != *file {
            continue;
        }
        tokens.push(SpannedToken {
            range: definition.name_range.clone(),
            token_type: symbol_kind_to_token_type(definition.kind),
            priority: TOKEN_AST_PRIORITY,
        });
    }
    for site in index.all_reference_sites() {
        if site.file != *file {
            continue;
        }
        tokens.push(SpannedToken {
            range: site.range.clone(),
            token_type: reference_token_type(&index, &site.name),
            priority: TOKEN_AST_PRIORITY,
        });
    }
    tokens
}

fn scan_lexical_tokens(source: &str) -> Vec<SpannedToken> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if byte == b'"' {
            let start = index;
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                    continue;
                }
                if bytes[index] == b'"' {
                    index += 1;
                    break;
                }
                index += 1;
            }
            tokens.push(SpannedToken {
                range: start..index,
                token_type: TOKEN_STRING,
                priority: 5,
            });
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            let start = index;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            tokens.push(SpannedToken {
                range: start..index,
                token_type: TOKEN_COMMENT,
                priority: 5,
            });
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            let start = index;
            let mut depth = 1;
            index += 2;
            while index < bytes.len() && depth > 0 {
                if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    depth -= 1;
                    index += 2;
                } else if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
                    depth += 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            tokens.push(SpannedToken {
                range: start..index,
                token_type: TOKEN_COMMENT,
                priority: 5,
            });
            continue;
        }
        if byte.is_ascii_digit() {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            if bytes.get(index) == Some(&b'.') && bytes.get(index + 1).is_some_and(|b| b.is_ascii_digit())
            {
                index += 1;
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    index += 1;
                }
            }
            tokens.push(SpannedToken {
                range: start..index,
                token_type: TOKEN_NUMBER,
                priority: 2,
            });
            continue;
        }
        if let Some((range, token_type)) = scan_operator(source, index) {
            tokens.push(SpannedToken {
                range: range.clone(),
                token_type,
                priority: 1,
            });
            index = range.end;
            continue;
        }
        if is_ident(byte) {
            let start = index;
            while index < bytes.len() && is_ident(bytes[index]) {
                index += 1;
            }
            let word = &source[start..index];
            let macro_call = bytes.get(index) == Some(&b'!') && bytes.get(index + 1) == Some(&b'(');
            let contextual_keyword = (word == "from" && is_yield_from_keyword(source, start))
                || (word == "with" && is_resume_with_keyword(source, start))
                || (word == "gen" && is_gen_fn_keyword(source, index));
            if coil_keywords().contains(&word)
                || highlight_keywords().contains(&word)
                || contextual_keyword
            {
                tokens.push(SpannedToken {
                    range: start..index,
                    token_type: TOKEN_KEYWORD,
                    priority: 3,
                });
            } else if macro_call {
                tokens.push(SpannedToken {
                    range: start..index,
                    token_type: TOKEN_MACRO,
                    priority: 6,
                });
            } else {
                let token_type = if is_type_like_ident(word) {
                    TOKEN_TYPE
                } else {
                    TOKEN_VARIABLE
                };
                tokens.push(SpannedToken {
                    range: start..index,
                    token_type,
                    priority: if token_type == TOKEN_TYPE { 3 } else { 1 },
                });
            }
            continue;
        }
        index += 1;
    }
    tokens
}

fn scan_operator(source: &str, index: usize) -> Option<(Range<usize>, u32)> {
    let bytes = source.as_bytes();
    let remaining = &bytes[index..];
    const MULTI: &[(&[u8], u32)] = &[
        (b"->", TOKEN_OPERATOR),
        (b"::", TOKEN_OPERATOR),
        (b"==", TOKEN_OPERATOR),
        (b"!=", TOKEN_OPERATOR),
        (b"<=", TOKEN_OPERATOR),
        (b">=", TOKEN_OPERATOR),
        (b"&&", TOKEN_OPERATOR),
        (b"||", TOKEN_OPERATOR),
        (b"**", TOKEN_OPERATOR),
        (b"<<", TOKEN_OPERATOR),
        (b">>", TOKEN_OPERATOR),
        (b"..", TOKEN_OPERATOR),
        (b"+=", TOKEN_OPERATOR),
        (b"-=", TOKEN_OPERATOR),
        (b"*=", TOKEN_OPERATOR),
        (b"/=", TOKEN_OPERATOR),
        (b"%=", TOKEN_OPERATOR),
        (b"^=", TOKEN_OPERATOR),
        (b"|=", TOKEN_OPERATOR),
        (b"&=", TOKEN_OPERATOR),
    ];
    for (pattern, token_type) in MULTI {
        if remaining.starts_with(pattern) {
            return Some((index..index + pattern.len(), *token_type));
        }
    }
    matches!(
        remaining[0],
        b'+' | b'-' | b'*' | b'/' | b'%' | b'<' | b'>' | b'=' | b'!' | b'&' | b'|' | b'^' | b'~'
            | b'.' | b',' | b';' | b':' | b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'@' | b'#'
    )
    .then_some((index..index + 1, TOKEN_OPERATOR))
}

fn is_type_like_ident(word: &str) -> bool {
    matches!(word, "int" | "float" | "string" | "bool" | "byte" | "void" | "unit")
        || word.chars().next().is_some_and(|character| character.is_uppercase())
}

fn is_yield_from_keyword(source: &str, from_start: usize) -> bool {
    let before = source[..from_start].trim_end();
    before.ends_with("yield")
}

fn is_resume_with_keyword(source: &str, with_start: usize) -> bool {
    let before = source[..with_start].trim_end();
    before.ends_with("resume")
}

/// `gen` is only a keyword directly before `fn`; elsewhere it is a name.
fn is_gen_fn_keyword(source: &str, gen_end: usize) -> bool {
    let after = source[gen_end..].trim_start();
    after.starts_with("fn") && !after[2..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
}

fn encode_semantic_tokens(source: &str, tokens: &[SpannedToken]) -> Vec<SemanticToken> {
    let mut encoded = Vec::new();
    let mut previous_line = 0;
    let mut previous_start = 0;
    for token in tokens {
        let position = byte_position(source, token.range.start);
        let length = source[token.range.clone()]
            .encode_utf16()
            .count() as u32;
        let delta_line = position.line - previous_line;
        let delta_start = if delta_line == 0 {
            position.character - previous_start
        } else {
            position.character
        };
        encoded.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: token.token_type,
            token_modifiers_bitset: 0,
        });
        previous_line = position.line;
        previous_start = position.character;
    }
    encoded
}

fn semantic_tokens(
    source: &str,
    file: Option<PathBuf>,
    filter: Option<Range<usize>>,
) -> Vec<SemanticToken> {
    let file = file.unwrap_or_else(|| PathBuf::from("untitled.hy"));
    let mut tokens = scan_lexical_tokens(source);
    if let Some(typed) = typed_semantic_tokens(source, &file) {
        tokens.extend(typed);
    } else if Pratt::default().parse(source).is_ok() {
        tokens.extend(ast_semantic_tokens(source, &file));
    }
    let merged = merge_spanned_tokens(tokens);
    let filtered = match filter {
        Some(filter) => merged
            .into_iter()
            .filter(|token| range_intersects(&filter, &token.range))
            .collect(),
        None => merged,
    };
    encode_semantic_tokens(source, &filtered)
}

/// True when `line` has a `name!(` function-style macro call.
fn has_macro_call(line: &str) -> bool {
    line.match_indices("!(").any(|(i, _)| {
        line[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranged_changes_splice_in_order() {
        let mut text = "fn main() {\n    let a = 1;\n}\n".to_string();
        let change = |range: Option<((u32, u32), (u32, u32))>, new: &str| {
            lsp_types::TextDocumentContentChangeEvent {
                range: range.map(|((sl, sc), (el, ec))| LspRange {
                    start: Position::new(sl, sc),
                    end: Position::new(el, ec),
                }),
                range_length: None,
                text: new.to_string(),
            }
        };
        apply_change(&mut text, change(Some(((1, 12), (1, 13))), "42"));
        apply_change(&mut text, change(Some(((1, 8), (1, 9))), "answer"));
        apply_change(&mut text, change(Some(((2, 1), (2, 1))), "\n// end"));
        assert_eq!(text, "fn main() {\n    let answer = 42;\n}\n// end\n");
        apply_change(&mut text, change(None, "fn main() {}\n"));
        assert_eq!(text, "fn main() {}\n");
    }

    #[test]
    fn options_parse_roots_and_grants_like_coil_compile() {
        let cwd = Path::new("/work/app");
        let args = [
            "--stdio",
            "--root",
            "vendor",
            "--root=/abs/lib",
            "--allow-exec",
            "--allow-exit",
            "--allow-dload",
            "sdl2",
            "--ffi-search-path=native",
        ]
        .map(String::from);
        let options = parse_options(args, cwd).unwrap().expect("not help");
        assert_eq!(
            options.extra_roots,
            vec![PathBuf::from("/work/app/vendor"), PathBuf::from("/abs/lib")]
        );
        assert!(options.grants.allow_exec && options.grants.allow_exit);
        assert!(!options.grants.allow_attach);
        assert_eq!(options.grants.allow_dload, vec!["sdl2".to_string()]);
        assert_eq!(options.grants.ffi_search_paths, vec![PathBuf::from("/work/app/native")]);
    }

    #[test]
    fn options_report_help_and_errors() {
        let cwd = Path::new("/");
        assert_eq!(parse_options(["--help".to_string()], cwd), Ok(None));
        assert_eq!(parse_options(Vec::new(), cwd), Ok(Some(LspOptions::default())));
        assert!(parse_options(["--root".to_string()], cwd).is_err());
        assert!(parse_options(["--bogus".to_string()], cwd).is_err());
    }

    #[test]
    fn cancellations_only_hit_queued_requests() {
        let cancel = |id: i32| {
            Message::Notification(Notification::new(
                "$/cancelRequest".into(),
                lsp_types::CancelParams {
                    id: lsp_types::NumberOrString::Number(id),
                },
            ))
        };
        let hover = |id: i32| Message::Request(Request::new(RequestId::from(id), "textDocument/hover".into(), Value::Null));
        let mut queue = VecDeque::from([hover(1), cancel(1), cancel(7), hover(2)]);
        assert_eq!(take_cancellations(&mut queue), [RequestId::from(1)]);
        assert_eq!(queue.len(), 2, "cancel notifications are consumed");
    }

    #[test]
    fn add_use_edit_joins_or_appends() {
        let joined = "use geo::{area};\n\nfn main() {}\n";
        let edit = add_use_edit(joined, "geo", "helper");
        assert_eq!(edit.new_text, ", helper");
        assert_eq!(edit.range.start, byte_position(joined, "use geo::{area".len()));

        let appended = "use util::{other};\n\nfn main() {}\n";
        let edit = add_use_edit(appended, "geo::shapes", "helper");
        assert_eq!(edit.new_text, "\nuse geo::shapes::{helper};");
        assert_eq!(edit.range.start, byte_position(appended, "use util::{other};".len()));

        let edit = add_use_edit("fn main() {}\n", "geo", "helper");
        assert_eq!(edit.new_text, "use geo::{helper};\n\n");
    }

    #[test]
    fn positions_use_utf16_columns() {
        assert_eq!(
            byte_position("α😀\nname", "α😀".len()),
            Position {
                line: 0,
                character: 3
            }
        );
    }

    #[test]
    fn occurrences_respect_identifier_boundaries() {
        assert_eq!(
            occurrences("foo foobar _foo foo", "foo"),
            vec![0..3, 16..19]
        );
    }

    #[test]
    fn diagnostics_include_type_errors() {
        let messages = analyze("fn main() { let x: int = true; }");
        assert!(!messages.is_empty());
    }

    #[test]
    fn completions_include_functions_with_docs_and_types() {
        let source = "\
/// Compute fibonacci
fn fib(int n) -> int {
    return n;
}
fn main() {
    fi
}
";
        // Mid-edit buffer with no prior snapshot: sanitize must recover decls.
        let document = Document {
            text: source.into(),
            version: 1,
            last_good: None,
        };
        let items = completions(
            &document,
            Position {
                line: 5,
                character: 6,
            },
        );
        let fib = items
            .iter()
            .find(|item| item.label == "fib")
            .expect("fib completion");
        assert_eq!(fib.kind, Some(CompletionItemKind::FUNCTION));
        assert!(
            fib.documentation
                .as_ref()
                .is_some_and(|docs| matches!(
                    docs,
                    Documentation::MarkupContent(MarkupContent { value, .. })
                        if value.contains("Compute fibonacci")
                )),
            "expected fib docs, got {:?}",
            fib.documentation
        );
        assert!(
            fib.detail
                .as_ref()
                .is_some_and(|detail| detail.contains("int")),
            "expected type detail, got {:?}",
            fib.detail
        );
        assert_eq!(fib.insert_text.as_deref(), Some("fib(${1:n})$0"));
        assert_eq!(fib.insert_text_format, Some(InsertTextFormat::SNIPPET));
        assert!(fib.command.is_some());
    }

    #[test]
    fn hover_includes_type_and_docs() {
        let document = Document {
            text: "\
/// Compute fibonacci
fn fib(int n) -> int {
    return n;
}
"
            .into(),
            version: 1,
            last_good: None,
        };
        let hover = hover(
            &document,
            Position {
                line: 1,
                character: 4,
            },
        )
        .expect("hover");
        let HoverContents::Markup(MarkupContent { value, .. }) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(value.contains("fib"));
        assert!(value.contains("Compute fibonacci"));
    }

    #[test]
    fn hover_shows_a_functions_effects() {
        let text = "\
static let HITS: int = 0;
fn bump(int x) -> int {
    HITS = HITS + 1;
    return x;
}
fn twice(int x, int -> int f) -> int {
    return f(f(x));
}
fn add2(int x) -> int {
    return twice(x, fn (int y) => y + 1);
}
";
        let value = |line: u32| {
            let document = Document {
                text: text.into(),
                version: 1,
                last_good: None,
            };
            let hover = hover(&document, Position { line, character: 4 }).expect("hover");
            let HoverContents::Markup(MarkupContent { value, .. }) = hover.contents else {
                panic!("expected markup hover");
            };
            value
        };
        let bump = value(1);
        assert!(bump.contains("**effects:** heap write, host state: writes static `HITS`"), "{bump}");
        let twice = value(5);
        assert!(twice.contains("**effects:** pure apart from its parameters: calls parameter `f`"), "{twice}");
        let add2 = value(8);
        assert!(add2.contains("**effects:** pure"), "{add2}");
    }

    #[test]
    fn hover_recovers_docs_when_buffer_has_incomplete_ident() {
        let document = Document {
            text: "\
/// Compute fibonacci
fn fib(int n) -> int {
    return n;
}
fn main() {
    fi
}
"
            .into(),
            version: 1,
            last_good: None,
        };
        let hover = hover(
            &document,
            Position {
                line: 1,
                character: 4,
            },
        )
        .expect("hover");
        let HoverContents::Markup(MarkupContent { value, .. }) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(
            value.contains("Compute fibonacci"),
            "expected docs in hover, got {value}"
        );
    }

    #[test]
    fn function_hover_includes_parameter_docs() {
        let document = Document {
            text: "\
/// Compute fibonacci
fn fib(
    /// Zero-based index.
    int n,
) -> int {
    return n;
}
"
            .into(),
            version: 1,
            last_good: None,
        };
        let hover = hover(
            &document,
            Position {
                line: 1,
                character: 4,
            },
        )
        .expect("function hover");
        let HoverContents::Markup(MarkupContent { value, .. }) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(value.contains("**Parameters**"), "hover was: {value}");
        assert!(value.contains("`n` (`int`): Zero-based index."));
    }

    #[test]
    fn parameter_hover_includes_parameter_docs() {
        let document = Document {
            text: "\
/// Compute fibonacci
fn fib(
    /// Zero-based index.
    int n,
) -> int {
    return n;
}
"
            .into(),
            version: 1,
            last_good: None,
        };
        let hover = hover(
            &document,
            Position {
                line: 3,
                character: 8,
            },
        )
        .expect("parameter hover");
        let HoverContents::Markup(MarkupContent { value, .. }) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(value.contains("Zero-based index."));
    }

    #[test]
    fn virtual_function_hover_and_completion_include_reference_docs() {
        let source = "use io::stdout;\nfn main() {\n    stdout();\n}\n";
        let document = Document {
            text: source.into(),
            version: 1,
            last_good: None,
        };
        let hover = hover(
            &document,
            Position {
                line: 0,
                character: 9,
            },
        )
        .expect("virtual function hover");
        let HoverContents::Markup(MarkupContent { value, .. }) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(value.contains("standard output"));
        assert!(value.contains("io.md"));

        let items = completions(
            &document,
            Position {
                line: 2,
                character: 10,
            },
        );
        let stdout = items
            .iter()
            .find(|item| item.label == "stdout")
            .expect("virtual completion");
        assert_eq!(stdout.kind, Some(CompletionItemKind::FUNCTION));
        assert!(stdout.documentation.is_some());
    }

    #[test]
    fn parameter_hover_in_condition_uses_binding_type() {
        let text = include_str!("../../examples/fib.hy");
        // `if n <= 2` — character 7 is the binding `n` (0-based).
        let line = text
            .lines()
            .position(|l| l.contains("if n <= 2"))
            .expect("`if n <= 2` in examples/fib.hy") as u32;
        let document = Document {
            text: text.into(),
            version: 1,
            last_good: None,
        };
        let hover = hover(
            &document,
            Position { line, character: 7 },
        )
        .expect("condition parameter hover");
        let HoverContents::Markup(MarkupContent { value, .. }) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(value.contains("n: int"), "condition hover was: {value}");
        assert!(!value.contains("never"), "condition hover was: {value}");
    }

    #[test]
    fn document_symbols_cover_top_level_decls() {
        let source = "\
use io::stdout as out;
type Id = int;
static let hits = 0;
enum Color { Red, Green }
class Point { pub x: int, pub y: int }
fn add(int a, int b) -> int { return a + b; }
";
        let symbols = document_symbols(source);
        let by_name: std::collections::HashMap<_, _> = symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind))
            .collect();
        assert_eq!(by_name.get("out"), Some(&lsp_types::SymbolKind::NAMESPACE));
        assert_eq!(
            by_name.get("Id"),
            Some(&lsp_types::SymbolKind::TYPE_PARAMETER)
        );
        assert_eq!(by_name.get("hits"), Some(&lsp_types::SymbolKind::VARIABLE));
        assert_eq!(by_name.get("Color"), Some(&lsp_types::SymbolKind::ENUM));
        assert_eq!(by_name.get("Point"), Some(&lsp_types::SymbolKind::CLASS));
        assert_eq!(by_name.get("add"), Some(&lsp_types::SymbolKind::FUNCTION));
    }

    #[test]
    fn document_symbols_cover_macro_and_derive_decls() {
        let source = "\
macro twice(Expr e) -> Code { return quote expr { ${e} }; }
derive Answer(TypeDecl t) -> Code { return quote items {}; }
";
        let symbols = document_symbols(source);
        let by_name: std::collections::HashMap<_, _> = symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind))
            .collect();
        assert_eq!(by_name.get("twice"), Some(&lsp_types::SymbolKind::FUNCTION));
        assert_eq!(by_name.get("Answer"), Some(&lsp_types::SymbolKind::FUNCTION));
    }

    #[test]
    fn signature_help_reports_active_parameter() {
        let source = "\
fn add(int a, int b) -> int { return a + b; }
fn main() {
    add(1, 
}
";
        let help = signature_help(
            &Document {
                text: source.into(),
                version: 1,
                last_good: None,
            },
            Position {
                line: 2,
                character: 11,
            },
        )
        .expect("signature help");
        assert_eq!(help.active_parameter, Some(1));
        let sig = &help.signatures[0];
        assert!(
            sig.label.contains("add"),
            "expected add signature, got {}",
            sig.label
        );
        assert_eq!(sig.parameters.as_ref().map(|p| p.len()), Some(2));
    }

    #[test]
    fn signature_help_function_style_macro_call() {
        let source = "\
macro twice(Expr e) -> Code { return quote expr { ${e} * 2 }; }
fn main() {
    twice!(
}
";
        let help = signature_help(
            &Document {
                text: source.into(),
                version: 1,
                last_good: None,
            },
            byte_position(source, source.find("twice!(").expect("call") + "twice!(".len()),
        )
        .expect("macro signature help");
        let sig = &help.signatures[0];
        assert!(
            sig.label.contains("twice"),
            "expected twice signature, got {}",
            sig.label
        );
        assert_eq!(sig.parameters.as_ref().map(|p| p.len()), Some(1));
    }

    fn token_types_at_word(source: &str, encoded: &[SemanticToken], word: &str) -> Vec<u32> {
        let mut line = 0u32;
        let mut character = 0u32;
        let mut types = Vec::new();
        for token in encoded {
            if token.delta_line > 0 {
                line += token.delta_line;
                character = token.delta_start;
            } else {
                character += token.delta_start;
            }
            let start = position_to_byte(
                source,
                Position {
                    line,
                    character,
                },
            )
            .expect("token start");
            let mut utf16 = 0u32;
            let mut end = start;
            for (offset, character) in source[start..].char_indices() {
                if utf16 >= token.length {
                    break;
                }
                utf16 += character.len_utf16() as u32;
                end = start + offset + character.len_utf8();
            }
            if &source[start..end] == word {
                types.push(token.token_type);
            }
        }
        types
    }

    fn token_at_byte<'a>(
        source: &str,
        encoded: &'a [SemanticToken],
        byte: usize,
    ) -> Option<&'a SemanticToken> {
        let mut line = 0u32;
        let mut character = 0u32;
        for token in encoded {
            if token.delta_line > 0 {
                line += token.delta_line;
                character = token.delta_start;
            } else {
                character += token.delta_start;
            }
            let start = position_to_byte(
                source,
                Position {
                    line,
                    character,
                },
            )
            .expect("token start");
            if start == byte {
                return Some(token);
            }
        }
        None
    }

    #[test]
    fn semantic_tokens_mark_comments_strings_and_numbers() {
        let source = "// comment\nfn main() { let x = \"hi\" + 42; return; }\n";
        let tokens = semantic_tokens(source, None, None);
        let types: Vec<_> = tokens.iter().map(|token| token.token_type).collect();
        assert!(types.contains(&TOKEN_COMMENT));
        assert!(types.contains(&TOKEN_STRING));
        assert!(types.contains(&TOKEN_NUMBER));
        assert!(types.contains(&TOKEN_KEYWORD));
        assert!(types.contains(&TOKEN_OPERATOR));
    }

    #[test]
    fn semantic_tokens_mark_block_comments_and_escaped_strings() {
        let source = "/* outer /* inner */ still */\nfn main() { let s = \"a\\\"//not\"; }\n";
        let tokens = semantic_tokens(source, None, None);
        assert!(
            token_types_at_word(source, &tokens, "still").is_empty(),
            "block comment body must not be a name token"
        );
        let comment = token_at_byte(source, &tokens, 0).expect("block comment");
        assert_eq!(comment.token_type, TOKEN_COMMENT);
        assert!(comment.length as usize >= "/* outer /* inner */ still */".len());
        let string_start = source.find("\"a\\").expect("string start");
        let inside = token_at_byte(source, &tokens, string_start).expect("string token");
        assert_eq!(inside.token_type, TOKEN_STRING);
    }

    #[test]
    fn semantic_tokens_four_slashes_are_line_comments_not_docs() {
        let source = "//// banner\nfn main() { return; }\n";
        let tokens = semantic_tokens(source, None, None);
        let banner = token_at_byte(source, &tokens, 0).expect("////");
        assert_eq!(banner.token_type, TOKEN_COMMENT);
        let kw = token_types_at_word(source, &tokens, "fn");
        assert_eq!(kw, vec![TOKEN_KEYWORD]);
    }

    #[test]
    fn semantic_tokens_mark_macro_keywords_and_calls() {
        let source =
            "macro twice(Expr e) -> Code { return quote expr { ${e} }; }\nfn main() { twice!(1); }\n";
        let tokens = semantic_tokens(source, Some(PathBuf::from("test.hy")), None);
        let kw = token_types_at_word(source, &tokens, "macro");
        assert_eq!(kw, vec![TOKEN_KEYWORD]);
        let quote = token_types_at_word(source, &tokens, "quote");
        assert_eq!(quote, vec![TOKEN_KEYWORD]);
        let decl = source.find("twice").expect("decl");
        let at_decl = token_at_byte(source, &tokens, decl).expect("macro name");
        assert_eq!(at_decl.token_type, TOKEN_MACRO);
        let call = source.rfind("twice").expect("call");
        let at_call = token_at_byte(source, &tokens, call).expect("twice!");
        assert_eq!(at_call.token_type, TOKEN_MACRO);
    }

    #[test]
    fn semantic_tokens_classify_function_declarations_and_calls() {
        let source = "fn fib(int n) -> int { return fib(n); }\n";
        let tokens = semantic_tokens(source, Some(PathBuf::from("test.hy")), None);
        let fib = token_types_at_word(source, &tokens, "fib");
        assert_eq!(fib.len(), 2);
        assert!(fib.iter().all(|token_type| *token_type == TOKEN_FUNCTION));
    }

    #[test]
    fn semantic_tokens_use_inferred_types_for_references() {
        let source = "\
fn fib(int n) -> int { return n; }
fn main() {
    let f = fib;
    return f(1);
}
";
        let tokens = semantic_tokens(source, Some(PathBuf::from("test.hy")), None);
        let call_f = token_types_at_word(source, &tokens, "f");
        assert!(
            call_f.contains(&TOKEN_FUNCTION),
            "expected function type at call site, got {call_f:?}"
        );
    }

    #[test]
    fn semantic_tokens_classify_type_names() {
        let source = "type Id = int;\nclass Point { pub x: int }\nfn main() { return; }\n";
        let tokens = semantic_tokens(source, Some(PathBuf::from("test.hy")), None);
        assert!(token_types_at_word(source, &tokens, "Point").contains(&TOKEN_TYPE));
        assert!(token_types_at_word(source, &tokens, "Id").contains(&TOKEN_TYPE));
        assert!(token_types_at_word(source, &tokens, "int").contains(&TOKEN_TYPE));
    }

    #[test]
    fn semantic_tokens_gen_is_keyword_only_before_fn() {
        let source = "gen fn count() -> int { yield 1; return 0; }\n";
        let tokens = semantic_tokens(source, Some(PathBuf::from("test.hy")), None);
        assert!(token_types_at_word(source, &tokens, "gen").contains(&TOKEN_KEYWORD));

        let source = "fn main() { let gen = 1; return; }\n";
        let tokens = semantic_tokens(source, Some(PathBuf::from("test.hy")), None);
        assert!(!token_types_at_word(source, &tokens, "gen").contains(&TOKEN_KEYWORD));
    }

    #[test]
    fn semantic_tokens_range_is_filtered() {
        let source = "fn left() { return; }\nfn right() { return; }\n";
        let full = semantic_tokens(source, None, None);
        let right_line_start = source.find("fn right").expect("right fn");
        let filtered = semantic_tokens(
            source,
            None,
            Some(right_line_start..source.len()),
        );
        assert!(filtered.len() < full.len());
        assert!(token_types_at_word(source, &filtered, "right").contains(&TOKEN_FUNCTION));
        assert!(token_types_at_word(source, &filtered, "left").is_empty());
    }

    #[test]
    fn semantic_tokens_use_utf16_lengths() {
        let source = "fn main() { let x = \"α\"; return; }\n";
        let tokens = semantic_tokens(source, None, None);
        let string_token = tokens
            .iter()
            .find(|token| token.token_type == TOKEN_STRING)
            .expect("string token");
        assert_eq!(string_token.length, 3);
    }

    #[test]
    fn semantic_tokens_classify_namespaces_and_enums() {
        let source = "\
use io::stdout as out;
enum Color { Red, Green }
fn main() { return; }
";
        let tokens = semantic_tokens(source, Some(PathBuf::from("test.hy")), None);
        assert!(
            token_types_at_word(source, &tokens, "out").contains(&TOKEN_NAMESPACE),
            "use alias should be namespace"
        );
        assert!(
            token_types_at_word(source, &tokens, "Color").contains(&TOKEN_TYPE),
            "enum decl should be type"
        );
    }

    #[test]
    fn semantic_tokens_mark_yield_from_keyword_but_not_bare_from() {
        let with_yield = "async fn f() { yield from inner(); }\n";
        let bare = "fn main() { let x = from; return; }\n";
        let yield_tokens = semantic_tokens(with_yield, None, None);
        let bare_tokens = semantic_tokens(bare, None, None);
        assert!(
            token_types_at_word(with_yield, &yield_tokens, "from").contains(&TOKEN_KEYWORD),
            "`from` after yield must be a keyword"
        );
        assert!(
            token_types_at_word(bare, &bare_tokens, "from")
                .iter()
                .all(|token_type| *token_type == TOKEN_VARIABLE),
            "bare `from` must not be keyword-colored"
        );
    }

    #[test]
    fn semantic_tokens_scan_floats_and_multi_char_operators() {
        let source = "fn id(int n) -> int { return Color::Red + 3.14; }\n";
        let tokens = semantic_tokens(source, None, None);
        let arrow = token_at_byte(source, &tokens, source.find("->").expect("arrow"))
            .expect("-> token");
        assert_eq!(arrow.token_type, TOKEN_OPERATOR);
        assert_eq!(arrow.length, 2);
        let path = token_at_byte(source, &tokens, source.find("::").expect("path"))
            .expect(":: token");
        assert_eq!(path.token_type, TOKEN_OPERATOR);
        assert_eq!(path.length, 2);
        let float = token_at_byte(source, &tokens, source.find("3.14").expect("float"))
            .expect("float token");
        assert_eq!(float.token_type, TOKEN_NUMBER);
        assert_eq!(float.length, 4);
    }

    #[test]
    fn semantic_tokens_typed_priority_overrides_pascal_case_heuristic() {
        // Top-level static so SymbolIndex records a Variable definition whose
        // inferred type (int) must beat the lexical PascalCase → type heuristic.
        let source = "\
static let Point = 1;
fn main() { return Point; }
";
        let tokens = semantic_tokens(source, Some(PathBuf::from("test.hy")), None);
        let point = token_types_at_word(source, &tokens, "Point");
        assert!(
            !point.is_empty(),
            "expected tokens for Point binding/use"
        );
        assert!(
            point.iter().all(|token_type| *token_type == TOKEN_VARIABLE),
            "typed int binding must beat PascalCase type heuristic, got {point:?}"
        );
    }

    #[test]
    fn semantic_tokens_classify_static_function_binding() {
        let source = "\
fn fib(int n) -> int { return n; }
static let f = fib;
fn main() { return f(1); }
";
        let tokens = semantic_tokens(source, Some(PathBuf::from("test.hy")), None);
        let f_types = token_types_at_word(source, &tokens, "f");
        assert!(
            f_types.contains(&TOKEN_FUNCTION),
            "static function-valued binding should highlight as function, got {f_types:?}"
        );
    }

    #[test]
    fn semantic_tokens_lexical_survive_parse_errors() {
        let source = "// broken\nfn {{{ \"hi\" 2.5\n";
        let tokens = semantic_tokens(source, None, None);
        let types: Vec<_> = tokens.iter().map(|token| token.token_type).collect();
        assert!(types.contains(&TOKEN_COMMENT));
        assert!(types.contains(&TOKEN_KEYWORD));
        assert!(types.contains(&TOKEN_STRING));
        assert!(types.contains(&TOKEN_NUMBER));
        assert!(types.contains(&TOKEN_OPERATOR));
    }

    fn write_hy_project(dir: &Path, files: &[(&str, &str)]) {
        std::fs::create_dir_all(dir).unwrap();
        for (name, src) in files {
            std::fs::write(dir.join(name), src).unwrap();
        }
    }

    fn open_state(dir: &Path, open: &[(&str, Option<&str>)]) -> (ServerState, HashMap<String, Uri>) {
        let mut state = ServerState {
            workspace_root: Some(dir.to_path_buf()),
            project_index: Some(ProjectIndex::with_roots(
                dir.to_path_buf(),
                vec![PathBuf::from(".")],
            )),
            ..ServerState::default()
        };
        let mut uris = HashMap::new();
        for (name, overlay) in open {
            let path = dir.join(name);
            let text = overlay
                .map(str::to_owned)
                .unwrap_or_else(|| std::fs::read_to_string(&path).unwrap());
            let uri = path_to_uri(&path).expect("uri");
            state.documents.insert(
                uri.clone(),
                Document {
                    text,
                    version: 1,
                    last_good: None,
                },
            );
            uris.insert((*name).to_owned(), uri);
        }
        if let Some((name, _)) = open.first() {
            refresh_project(&mut state, &dir.join(name));
        }
        (state, uris)
    }

    #[test]
    fn goto_definition_reaches_unopened_import() {
        let dir = std::env::temp_dir().join(format!(
            "coil-lsp-goto-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        write_hy_project(
            &dir,
            &[
                ("lib.hy", "fn helper() -> int { return 1; }\n"),
                (
                    "main.hy",
                    "use lib::helper;\nfn main() { let x = helper(); return; }\n",
                ),
            ],
        );
        let (state, uris) = open_state(&dir, &[("main.hy", None)]);
        let main = std::fs::read_to_string(dir.join("main.hy")).unwrap();
        let offset = main.find("helper()").expect("call");
        let position = byte_position(&main, offset);
        let locations = goto_definitions(&state, &uris["main.hy"], position);
        assert_eq!(locations.len(), 1, "{locations:?}");
        assert!(
            locations[0].uri.to_string().contains("lib.hy"),
            "expected unopened lib.hy, got {:?}",
            locations[0].uri
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overlay_diagnostics_see_unsaved_type_error() {
        let dir = std::env::temp_dir().join(format!(
            "coil-lsp-overlay-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        write_hy_project(
            &dir,
            &[
                ("lib.hy", "fn helper() -> int { return 1; }\n"),
                (
                    "main.hy",
                    "use lib::helper;\nfn main() { let x: int = helper(); return; }\n",
                ),
            ],
        );
        let overlay = "use lib::helper;\nfn main() { let x: bool = helper(); return; }\n";
        let (state, uris) = open_state(&dir, &[("main.hy", Some(overlay))]);
        let diagnostics = project_diagnostics(&state, &uris["main.hy"], &state.documents[&uris["main.hy"]]);
        assert!(
            diagnostics.iter().any(|d| d.message.contains("bool") || d.message.contains("int")),
            "expected unsaved type error, got {diagnostics:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_skips_comments_and_strings() {
        let source = "// helper leftover\nfn helper() { return; }\nfn main() { let s = \"helper\"; helper(); return; }\n";
        let mut state = ServerState::default();
        let uri: Uri = "file:///tmp/rename.hy".parse().unwrap();
        state.documents.insert(
            uri.clone(),
            Document {
                text: source.into(),
                version: 1,
                last_good: None,
            },
        );
        let offset = source.rfind("helper()").expect("call");
        let edits = rename_identifier(
            &state,
            &uri,
            byte_position(source, offset),
            "other",
        );
        let file_edits = edits.changes.unwrap().remove(&uri).expect("edits");
        let replaced: Vec<_> = file_edits
            .iter()
            .map(|edit| {
                let range = lsp_range_to_byte_range(source, edit.range).unwrap();
                source[range].to_owned()
            })
            .collect();
        assert!(replaced.iter().all(|word| word == "helper"));
        assert_eq!(replaced.len(), 2, "decl + call, not comment/string: {replaced:?}");
    }

    #[test]
    fn range_format_rewrites_only_overlapping_item() {
        let source = "fn a(){return;}\nfn b(){return;}\n";
        let a_end = source.find('\n').unwrap();
        let edits = format_requested_range(source, 0..a_end).expect("edits");
        assert_eq!(edits.len(), 1);
        assert!(edits[0].new_text.contains("fn a"));
        assert!(!edits[0].new_text.contains("fn b"));
    }

    #[test]
    fn selection_range_nests_inside_function() {
        let source = "fn main() { return 1; }\n";
        let offset = source.find('1').unwrap();
        let ranges = selection_ranges(source, &[byte_position(source, offset)]);
        assert_eq!(ranges.len(), 1);
        assert!(ranges[0].parent.is_some(), "expected nested parent spans");
        assert!(ranges[0].range.end.character > ranges[0].range.start.character
            || ranges[0].range.end.line >= ranges[0].range.start.line);
    }

    #[test]
    fn clock_path_completion_and_hover() {
        let source = "use clock::wall_nanos;\nfn main() { wall_nanos(); return; }\n";
        let document = Document {
            text: source.into(),
            version: 1,
            last_good: None,
        };
        let items = completions(
            &document,
            Position {
                line: 0,
                character: 11,
            },
        );
        assert!(
            items.iter().any(|item| item.label == "wall_nanos"),
            "expected clock:: members, got {:?}",
            items.iter().map(|i| i.label.clone()).collect::<Vec<_>>()
        );
        let hover = hover(
            &document,
            Position {
                line: 0,
                character: 14,
            },
        )
        .expect("clock hover");
        let HoverContents::Markup(MarkupContent { value, .. }) = hover.contents else {
            panic!("markup");
        };
        assert!(
            value.contains("HostInvoke") || value.contains("wall"),
            "clock hover was {value}"
        );
        assert!(!value.contains("references/time.md"));
    }

    #[test]
    fn enum_variant_completion_after_dot() {
        let source = "enum Color { Red, Green }\nfn main() { let c = Color.; return; }\n";
        let document = Document {
            text: source.into(),
            version: 1,
            last_good: None,
        };
        let items = completions(
            &document,
            byte_position(source, source.find("Color.").expect("dot") + "Color.".len()),
        );
        let red = items
            .iter()
            .find(|item| item.label == "Color.Red" || item.insert_text.as_deref() == Some("Red"))
            .expect("Color.Red");
        assert_eq!(red.kind, Some(CompletionItemKind::ENUM_MEMBER));
        assert_eq!(red.insert_text.as_deref(), Some("Red"));
    }

    #[test]
    fn signature_help_virtual_import() {
        let source = "use io::stdout;\nfn main() { stdout( }\n";
        let help = signature_help(
            &Document {
                text: source.into(),
                version: 1,
                last_good: None,
            },
            Position {
                line: 1,
                character: 20,
            },
        )
        .expect("virtual signature");
        assert!(
            help.signatures[0].label.contains("stdout")
                || help.signatures[0]
                    .documentation
                    .as_ref()
                    .is_some(),
            "{help:?}"
        );
    }

    #[test]
    fn match_hover_walks_scrutinee() {
        let source = "\
fn main() {
    let value = Option.Some(1);
    match value {
        Option.Some(n) => { return n; }
        Option.None => { return 0; }
    }
}
";
        let document = Document {
            text: source.into(),
            version: 1,
            last_good: None,
        };
        let offset = source.find("match value").expect("scrutinee") + "match ".len();
        let hover = hover(&document, byte_position(source, offset)).expect("hover");
        let HoverContents::Markup(MarkupContent { value, .. }) = hover.contents else {
            panic!("markup");
        };
        assert!(value.contains("value"), "match hover was {value}");
    }

    #[test]
    fn uri_percent_decodes_spaces() {
        let uri: Uri = "file:///tmp/my%20pkg/src/lib.hy".parse().unwrap();
        let path = uri_path(&uri).expect("path");
        assert!(path.ends_with("my pkg/src/lib.hy"), "{path:?}");
    }

    fn state_with(source: &str) -> (ServerState, Uri) {
        let mut state = ServerState::default();
        let uri: Uri = "file:///tmp/coil-lsp-test.hy".parse().unwrap();
        state.documents.insert(
            uri.clone(),
            Document {
                text: source.into(),
                version: 1,
                last_good: None,
            },
        );
        (state, uri)
    }

    fn renamed_ranges(source: &str, edits: lsp_types::WorkspaceEdit, uri: &Uri) -> Vec<Range<usize>> {
        let mut ranges: Vec<_> = edits
            .changes
            .unwrap_or_default()
            .remove(uri)
            .unwrap_or_default()
            .iter()
            .map(|edit| lsp_range_to_byte_range(source, edit.range).unwrap())
            .collect();
        ranges.sort_by_key(|r| r.start);
        ranges
    }

    #[test]
    fn rename_local_includes_declaration_and_stays_in_scope() {
        let source = "fn a() {\n    let p = 1;\n    let _ = p;\n}\nfn b() {\n    let p = 2;\n    let _ = p;\n}\n";
        let (state, uri) = state_with(source);
        let use_offset = source.find("_ = p").unwrap() + 4;
        let edits = rename_identifier(&state, &uri, byte_position(source, use_offset), "q");
        let decl = source.find("let p").unwrap() + 4;
        assert_eq!(
            renamed_ranges(source, edits, &uri),
            vec![decl..decl + 1, use_offset..use_offset + 1],
            "only fn a's `p`, declaration included"
        );
    }

    #[test]
    fn rename_global_skips_same_named_local() {
        let source = "fn helper() -> int { return 1; }\nfn main() {\n    let _ = helper();\n}\nfn other() {\n    let helper = 2;\n    let _ = helper;\n}\n";
        let (state, uri) = state_with(source);
        let call = source.find("helper()").unwrap();
        let edits = rename_identifier(&state, &uri, byte_position(source, call), "util");
        let ranges = renamed_ranges(source, edits, &uri);
        assert_eq!(ranges.len(), 2, "decl + call only: {ranges:?}");
        assert!(ranges.iter().all(|r| r.end <= source.find("fn other").unwrap()));
    }

    #[test]
    fn identifier_validation_rejects_keywords_and_junk() {
        assert!(is_valid_identifier("value_2"));
        assert!(!is_valid_identifier("2value"));
        assert!(!is_valid_identifier("let"));
        assert!(!is_valid_identifier("self"));
        assert!(!is_valid_identifier("a-b"));
        assert!(!is_valid_identifier(""));
    }

    #[test]
    fn hover_type_of_let_uses_initializer_not_statement() {
        let source = "fn two() -> int { return 2; }\nfn main() {\n    let s = two();\n    let t: string = \"x\";\n}\n";
        let ast = Pratt::default().parse(source).unwrap();
        let mut checker = Checker::new();
        let _ = checker.check_program(&ast);
        let at = |needle: &str| {
            let offset = source.find(needle).unwrap() + 4;
            let range = offset..offset + 1;
            hover_type(&ast, Some(&checker), source, &source[range.clone()], offset, &range)
        };
        assert_eq!(at("let s").as_deref(), Some("int"));
        assert_eq!(at("let t").as_deref(), Some("string"));
    }

    #[test]
    fn call_paren_scan_handles_nesting_strings_and_commas() {
        assert_eq!(enclosing_call_paren("f(a, g(b), "), Some((1, 2)));
        assert_eq!(enclosing_call_paren("f(g(x"), Some((3, 0)));
        assert_eq!(enclosing_call_paren("f(\"(,\", "), Some((1, 1)));
        assert_eq!(enclosing_call_paren("f(a)"), None);
        assert_eq!(enclosing_call_paren("f([1, "), None);
        assert_eq!(callee_name_before_paren("twice!(1, ", 6), Some((0, "twice")));
        assert_eq!(callee_name_before_paren("add(1, ", 3), Some((0, "add")));
    }

    #[test]
    fn signature_help_reads_declaration_with_return_type() {
        let source = "fn add(int a, int b) -> int { return a + b; }\nfn main() {\n    let _ = add(1, add(2, \n}\n";
        let (state, uri) = state_with(source);
        let offset = source.find("add(2, ").unwrap() + "add(2, ".len();
        let help = decl_signature_help(&state, &uri, byte_position(source, offset)).expect("help");
        assert_eq!(help.signatures[0].label, "add(int a, int b) -> int");
        assert_eq!(help.active_parameter, Some(1));
    }
}
