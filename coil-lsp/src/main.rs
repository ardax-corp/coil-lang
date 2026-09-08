//! Coil language server. The transport is standard LSP JSON-RPC over stdio.
#![allow(deprecated)]

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
};

use compiler::{
    BuiltinExport, Checker, ProjectIndex, SymbolIndex, SymbolKind, VirtualModules,
    format_ty_for_diag,
};
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
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
}

fn main() {
    if let Err(error) = run() {
        eprintln!("coil-lsp: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (connection, io_threads) = Connection::stdio();
    let (request_id, initialize_params) = connection.initialize_start()?;
    let _params: InitializeParams = serde_json::from_value(initialize_params)?;
    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
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
        rename_provider: Some(lsp_types::OneOf::Left(true)),
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

    let mut state = ServerState::default();
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
        let index = ProjectIndex::with_roots(root_uri.clone(), lsp_module_roots(&root_uri));
        state.project_index = Some(index);
    }
    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if request.method == "shutdown" {
                    send_response(&connection, request.id, Value::Null)?;
                    continue;
                }
                if let Some(value) = handle_request(&mut state, &request)? {
                    send_response(&connection, request.id, value)?;
                }
            }
            Message::Notification(notification) => {
                if notification.method == "exit" {
                    break;
                }
                handle_notification(&connection, &mut state, &notification)?;
            }
            Message::Response(_) => {}
        }
    }
    io_threads.join()?;
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
        "textDocument/hover" => {
            let params: HoverParams = serde_json::from_value(request.params.clone())?;
            let hover = state
                .documents
                .get(&params.text_document_position_params.text_document.uri)
                .and_then(|document| {
                    hover(document, params.text_document_position_params.position)
                });
            Some(serde_json::to_value(hover)?)
        }
        "textDocument/completion" => {
            let params: CompletionParams = serde_json::from_value(request.params.clone())?;
            let items = state
                .documents
                .get(&params.text_document_position.text_document.uri)
                .map(|document| {
                    completions(document, params.text_document_position.position)
                })
                .unwrap_or_default();
            Some(serde_json::to_value(items)?)
        }
        "textDocument/signatureHelp" => {
            let params: lsp_types::SignatureHelpParams =
                serde_json::from_value(request.params.clone())?;
            let signature = state
                .documents
                .get(&params.text_document_position_params.text_document.uri)
                .and_then(|document| {
                    signature_help(document, params.text_document_position_params.position)
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
        "textDocument/rename" => {
            let params: lsp_types::RenameParams = serde_json::from_value(request.params.clone())?;
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
            if let Some(document) = state.documents.get_mut(&params.text_document.uri) {
                if let Some(change) = params.content_changes.into_iter().next() {
                    document.text = change.text;
                    document.version = params.text_document.version;
                    let offset = document.text.len().saturating_sub(1);
                    if let Some(good) =
                        analyze_for_completions_at(&document.text, Some(offset))
                    {
                        document.last_good = Some(good);
                    }
                }
                if let Some(path) = uri_path(&params.text_document.uri) {
                    refresh_project(state, &path);
                }
                publish_diagnostics(connection, state, &params.text_document.uri)?;
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
            if let Some(path) = uri_path(&params.text_document.uri) {
                if let Some(index) = state.project_index.as_mut() {
                    index.pipeline_mut().clear_file_text(&path);
                }
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

fn publish_diagnostics(
    connection: &Connection,
    state: &ServerState,
    uri: &Uri,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(document) = state.documents.get(uri) else {
        return Ok(());
    };
    let diagnostics = project_diagnostics(state, uri, document);
    connection
        .sender
        .send(Message::Notification(Notification::new(
            "textDocument/publishDiagnostics".into(),
            PublishDiagnosticsParams {
                uri: uri.clone(),
                diagnostics,
                version: Some(document.version),
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
    checker.take_messages()
}

fn lsp_module_roots(workspace: &Path) -> Vec<PathBuf> {
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
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                out.push(value);
                index += 3;
                continue;
            }
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
            if let Some(location) = location_for_file(state, path, &site.range) {
                push(location);
            }
        }
    };

    if let Some(project) = &state.project_index {
        for path in project.indexed_paths() {
            if let Some(source) = project.source_for(path) {
                if let Some(index) = project.symbols_for(path) {
                    visit_index(path, source, index);
                }
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
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for location in locations {
        changes
            .entry(location.uri)
            .or_default()
            .push(TextEdit {
                range: location.range,
                new_text: new_name.to_owned(),
            });
    }
    lsp_types::WorkspaceEdit {
        changes: Some(changes),
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
    for keyword in coil_keywords() {
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
            if let Some(parameters) = parameter_docs_markdown(args) {
                if let Some(candidate) = out.get_mut(*name) {
                    let base = candidate.documentation.take().unwrap_or_default();
                    candidate.documentation = Some(format!("{base}{parameters}"));
                }
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
        Expression::AttrDecl { name, docs, .. } => {
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
        Expression::Function { body, .. } => {
            if let Some(body) = body {
                collect_virtual_candidates(body.1.as_ref(), modules, out);
            }
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
    let description = match (module, name) {
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
        ("io", "await_readable") => "Parks until the stream is readable (yields inside a coroutine).".into(),
        ("io", "await_writable") => "Parks until the stream is writable (yields inside a coroutine).".into(),
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
    };
    description
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

fn hover(document: &Document, position: Position) -> Option<Hover> {
    let offset = position_to_byte(&document.text, position)?;
    let range = word_range(&document.text, offset)?;
    let name = document.text.get(range.clone())?.to_owned();

    if let Some(hover) = hover_from_source(&document.text, &name, offset, range.clone()) {
        if hover_has_detail(&hover) {
            return Some(hover);
        }
    }

    // Mid-edit buffers often fail to parse because of an incomplete *other*
    // token (e.g. `fi` in `main` while hovering `fib`). Repair near EOF, not
    // under the hover word (which would delete the declaration being inspected).
    let repair_offset = document.text.len().saturating_sub(1);
    if repair_offset < range.start || repair_offset >= range.end {
        for sanitized in sanitize_variants(&document.text, repair_offset) {
            if let Some(hover) = hover_from_source(&sanitized, &name, offset, range.clone()) {
                if hover_has_detail(&hover) {
                    return Some(hover);
                }
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
    hover_markup(source, name, range, ty_text, docs)
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
            if let Expression::Fragment(items) = args.1.as_ref() {
                if let Some(docs) = items.iter().find_map(|(_, item)| {
                    let Expression::Argument {
                        docs,
                        name: param_name,
                        ..
                    } = item.as_ref()
                    else {
                        return None;
                    };
                    (*param_name == name && !docs.is_empty())
                        .then(|| docs_markdown(&docs))
                }) {
                    return docs;
                }
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

fn signature_help(document: &Document, position: Position) -> Option<SignatureHelp> {
    let source = &document.text;
    let offset = position_to_byte(source, position)?;
    let prefix = &source[..offset.min(source.len())];
    let open = prefix.rfind('(')?;
    let name_end = prefix[..open].trim_end().len();
    let name_start = prefix[..name_end]
        .rfind(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .map(|index| index + 1)
        .unwrap_or(0);
    let name = &prefix[name_start..name_end];
    if source.find(&format!("fn {name}")).is_none() {
        return signature_help_from_index(document, name, prefix, open);
    }
    let declaration = source.find(&format!("fn {name}"))?;
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
    if let Ok(ast) = Pratt::default().parse(source) {
        if let Some(docs) = function_parameter_docs(ast.1.as_ref(), name) {
            for (parameter, docs) in parameters.iter_mut().zip(docs) {
                if let Some(docs) = docs {
                    parameter.documentation = Some(Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: docs,
                    }));
                }
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
            if let Expression::Fragment(items) = args.1.as_ref() {
                if let Some(ty) = items.iter().find_map(|(_, item)| {
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
                            Some(docs_markdown(&docs))
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

fn coil_keywords() -> &'static [&'static str] {
    &[
        "fn", "let", "const", "class", "enum", "type", "if", "else", "for", "while", "in",
        "match", "return", "true", "false", "use", "mod", "pub", "static", "async", "defer",
        "raise", "panic", "yield", "break", "continue", "where", "impl", "trait", "extern", "as",
        "readonly", "new", "default", "typeof", "resume", "with", "done",
    ]
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
        SymbolKind::Variable => checker
            .codegen_var_type(&definition.name)
            .map(|ty| semantic_token_type_for_ty(ty))
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
            while index < bytes.len() && bytes[index] != b'"' {
                index += 1;
            }
            if index < bytes.len() {
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
            if coil_keywords().contains(&word) {
                tokens.push(SpannedToken {
                    range: start..index,
                    token_type: TOKEN_KEYWORD,
                    priority: 3,
                });
            } else if word == "from" && is_yield_from_keyword(source, start) {
                tokens.push(SpannedToken {
                    range: start..index,
                    token_type: TOKEN_KEYWORD,
                    priority: 3,
                });
            } else if word == "with" && is_resume_with_keyword(source, start) {
                tokens.push(SpannedToken {
                    range: start..index,
                    token_type: TOKEN_KEYWORD,
                    priority: 3,
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
            | b'.' | b',' | b';' | b':' | b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'@'
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let document = Document {
            text: include_str!("../../examples/fib.hy").into(),
            version: 1,
            last_good: None,
        };
        let hover = hover(
            &document,
            Position {
                // `if n <= 2` — character 7 is the binding `n` (0-based).
                line: 10,
                character: 7,
            },
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
        assert!(token_types_at_word(source, &filtered, "right")
            .iter()
            .any(|token_type| *token_type == TOKEN_FUNCTION));
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
}
