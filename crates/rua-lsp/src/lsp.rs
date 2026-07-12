//! `rua-lsp` — the Rua Language Server.
//!
//! Communicates via stdio JSON-RPC (LSP). All semantic queries go through a
//! single long-lived [`AnalysisHost`]; there is no legacy workspace or compiler
//! bridge in the production path.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeConfiguration, DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument,
    DidOpenTextDocument, DidSaveTextDocument, Notification as _, PublishDiagnostics,
};
use lsp_types::request::{
    CallHierarchyIncomingCalls, CallHierarchyOutgoingCalls, CallHierarchyPrepare,
    CodeActionRequest, CodeLensRequest, Completion, DocumentHighlightRequest,
    DocumentLinkRequest, DocumentSymbolRequest, ExecuteCommand, FoldingRangeRequest,
    Formatting, GotoDefinition, GotoImplementation, HoverRequest, InlayHintRequest,
    OnTypeFormatting, PrepareRenameRequest, RangeFormatting, References, Rename,
    Request as _, ResolveCompletionItem, SelectionRangeRequest, SemanticTokensFullRequest,
    SemanticTokensRangeRequest, SignatureHelpRequest, TypeHierarchyPrepare,
    TypeHierarchySubtypes, TypeHierarchySupertypes, WorkspaceSymbolRequest,
};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionParams, CodeActionResponse, CodeLens,
    CodeLensParams, CompletionItem, CompletionItemKind, CompletionList, CompletionOptions,
    CompletionResponse, Diagnostic, DiagnosticSeverity, DidChangeWatchedFilesParams,
    DocumentFormattingParams, DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams,
    DocumentLink, DocumentLinkParams, DocumentOnTypeFormattingParams, DocumentSymbol,
    DocumentSymbolResponse, Documentation, FileSystemWatcher, FoldingRange, FoldingRangeKind,
    FoldingRangeParams, GotoDefinitionResponse, Hover, HoverContents, HoverProviderCapability,
    InitializeParams, InlayHint, InlayHintKind, InlayHintLabel, InlayHintLabelPart,
    InlayHintParams,
    InsertTextFormat, Location, MarkupContent, MarkupKind, OneOf, ParameterInformation,
    ParameterLabel, Position, PrepareRenameResponse, PublishDiagnosticsParams, Range,
    Registration, RegistrationParams, RenameOptions, SelectionRange, SelectionRangeParams,
    ServerCapabilities, SemanticToken as LspSemanticToken, SemanticTokenModifier,
    SemanticTokenType, SemanticTokens, SemanticTokensFullOptions, SemanticTokensLegend,
    SemanticTokensOptions, SemanticTokensResult, SemanticTokensServerCapabilities,
    SignatureHelp, SignatureHelpOptions, SignatureInformation,
    SymbolKind as LspSymbolKind, TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit,
    Uri, WatchKind, WorkspaceEdit, WorkspaceSymbol, WorkspaceSymbolParams,
    WorkspaceSymbolResponse,
};

use rua_analysis::{
    AnalysisHost, Change, CompletionInsert, CompletionKind, DefKind, FileId, FileKind,
    HoverResult, MacroDelimiter, NavigationTarget, ProjectId, ProjectPosition,
    SemanticTokenKind, SourceChange, SourceRootId, SourceRootKind, TextRange,
};
use rua_syntax::LineIndex;

// ---------------------------------------------------------------------------
// Server state
// ---------------------------------------------------------------------------

struct Server {
    connection: Connection,
    /// Single authoritative semantic state.
    host: AnalysisHost,
    /// URI → (path, FileId) mapping. The path key is canonical.
    file_ids: HashMap<PathBuf, (Uri, FileId)>,
    /// FileId → (URI, open buffer text).
    open_buffers: HashMap<FileId, (Uri, String)>,
    /// FileId → URI reverse lookup, maintained alongside `file_ids`
    /// and `open_buffers` so rename and code-edit handlers don't
    /// need to rebuild it from scratch.
    file_to_uri: HashMap<FileId, Uri>,
    /// Next FileId to allocate.
    next_file_id: u32,
    /// Next SourceRootId to allocate.
    next_root_id: u32,
    /// library config state
    library_roots: Vec<PathBuf>,
    library_mounts: HashMap<String, PathBuf>,
    /// Paths currently watched via `workspace/didChangeWatchedFiles`.
    watched_paths: Vec<PathBuf>,
    /// Timestamp of last watcher-triggered reload (debounce, 100ms).
    last_watcher_event: Option<std::time::Instant>,
}

impl Server {
    fn new(connection: Connection) -> Self {
        Server {
            connection,
            host: AnalysisHost::new(),
            file_ids: HashMap::new(),
            open_buffers: HashMap::new(),
            file_to_uri: HashMap::new(),
            next_file_id: 0,
            next_root_id: 1, // 0 is workspace root
            library_roots: Vec::new(),
            library_mounts: HashMap::new(),
            watched_paths: Vec::new(),
            last_watcher_event: None,
        }
    }

    // -- file identity -------------------------------------------------------

    fn doc_key(uri: &Uri) -> PathBuf {
        uri_to_path(uri).unwrap_or_else(|| PathBuf::from(uri.as_str()))
    }

    fn file_id_for_uri(&self, uri: &Uri) -> Option<FileId> {
        let key = Self::doc_key(uri);
        self.file_ids.get(&key).map(|(_, id)| *id)
    }

    fn uri_for_file(&self, file_id: FileId) -> Option<Uri> {
        self.file_to_uri.get(&file_id).cloned()
    }

    fn ensure_file_id(&mut self, uri: &Uri) -> FileId {
        let key = Self::doc_key(uri);
        if let Some((_, id)) = self.file_ids.get(&key) {
            return *id;
        }
        let id = FileId::new(self.next_file_id);
        self.next_file_id += 1;
        self.file_ids.insert(key, (uri.clone(), id));
        self.file_to_uri.insert(id, uri.clone());
        id
    }

    // -- main loop -----------------------------------------------------------

    fn main_loop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            let msg = match self.connection.receiver.recv() {
                Ok(m) => m,
                Err(_) => return Ok(()),
            };
            match msg {
                Message::Request(req) => {
                    if self.connection.handle_shutdown(&req)? {
                        return Ok(());
                    }
                    self.handle_request(req);
                }
                Message::Notification(not) => {
                    self.handle_notification(not);
                }
                Message::Response(_resp) => {}
            }
        }
    }

    // -- requests ------------------------------------------------------------

    fn handle_request(&mut self, req: Request) {
        match req.method.as_str() {
            Formatting::METHOD => self.handle_formatting(req),
            RangeFormatting::METHOD => self.handle_range_formatting(req),
            SelectionRangeRequest::METHOD => self.handle_selection_range(req),
            CodeLensRequest::METHOD => self.handle_code_lens(req),
            HoverRequest::METHOD => self.handle_hover(req),
            GotoDefinition::METHOD => self.handle_definition(req),
            GotoImplementation::METHOD => self.handle_goto_implementation(req),
            DocumentSymbolRequest::METHOD => self.handle_document_symbol(req),
            Completion::METHOD => self.handle_completion(req),
            References::METHOD => self.handle_references(req),
            Rename::METHOD => self.handle_rename(req),
            PrepareRenameRequest::METHOD => self.handle_prepare_rename(req),
            SemanticTokensFullRequest::METHOD => self.handle_semantic_tokens(req),
            SemanticTokensRangeRequest::METHOD => self.handle_semantic_tokens_range(req),
            ResolveCompletionItem::METHOD => self.handle_resolve_completion(req),
            SignatureHelpRequest::METHOD => self.handle_signature_help(req),
            InlayHintRequest::METHOD => self.handle_inlay_hint(req),
            DocumentHighlightRequest::METHOD => self.handle_document_highlight(req),
            CodeActionRequest::METHOD => self.handle_code_action(req),
            ExecuteCommand::METHOD => self.handle_execute_command(req),
            CallHierarchyPrepare::METHOD => self.handle_call_hierarchy_prepare(req),
            CallHierarchyIncomingCalls::METHOD => self.handle_call_hierarchy_incoming(req),
            CallHierarchyOutgoingCalls::METHOD => self.handle_call_hierarchy_outgoing(req),
            TypeHierarchyPrepare::METHOD => self.handle_type_hierarchy_prepare(req),
            TypeHierarchySubtypes::METHOD => self.handle_type_hierarchy_subtypes(req),
            TypeHierarchySupertypes::METHOD => self.handle_type_hierarchy_supertypes(req),
            WorkspaceSymbolRequest::METHOD => self.handle_workspace_symbol(req),
            FoldingRangeRequest::METHOD => self.handle_folding_range(req),
            DocumentLinkRequest::METHOD => self.handle_document_link(req),
            OnTypeFormatting::METHOD => self.handle_on_type_formatting(req),
            _ => {
                let resp = Response::new_err(
                    req.id,
                    lsp_server::ErrorCode::MethodNotFound as i32,
                    format!("unknown request: {}", req.method),
                );
                let _ = self.connection.sender.send(Message::Response(resp));
            }
        }
    }

    // -- formatting ----------------------------------------------------------

    fn handle_formatting(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params) = match req.extract::<DocumentFormattingParams>(Formatting::METHOD) {
            Ok(v) => v,
            Err(e) => {
                let resp = Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    format!("invalid formatting params: {e:?}"),
                );
                let _ = self.connection.sender.send(Message::Response(resp));
                return;
            }
        };
        let key = Self::doc_key(&params.text_document.uri);
        let edits = if let Some((_, text)) =
            self.file_id_for_uri(&params.text_document.uri)
                .and_then(|id| self.open_buffers.get(&id))
        {
            format_edits(text)
        } else {
            // Read from disk if not open (via VFS analysis)
            let file_id = self.ensure_file_id(&params.text_document.uri);
            let analysis = self.host.analysis();
            let text = analysis.parse(file_id).syntax_node().text().to_string();
            format_edits(&text)
        };
        drop(key);
        let result: Option<Vec<TextEdit>> =
            if edits.is_empty() { None } else { Some(edits) };
        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- range formatting ----------------------------------------------------

    fn handle_range_formatting(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params) = match req
            .extract::<lsp_types::DocumentRangeFormattingParams>(RangeFormatting::METHOD)
        {
            Ok(v) => v,
            Err(e) => {
                let resp = Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    format!("invalid range-formatting params: {e:?}"),
                );
                let _ = self.connection.sender.send(Message::Response(resp));
                return;
            }
        };
        // Format the whole document but only return edits within the range.
        let all_edits = {
            let key = Self::doc_key(&params.text_document.uri);
            let edits = if let Some((_, text)) = self
                .file_id_for_uri(&params.text_document.uri)
                .and_then(|id| self.open_buffers.get(&id))
            {
                format_edits(text)
            } else {
                let file_id = self.ensure_file_id(&params.text_document.uri);
                let analysis = self.host.analysis();
                let text = analysis.parse(file_id).syntax_node().text().to_string();
                format_edits(&text)
            };
            drop(key);
            edits
        };
        let range = &params.range;
        let filtered: Vec<TextEdit> = all_edits
            .into_iter()
            .filter(|edit| ranges_overlap_lsp(&edit.range, range))
            .collect();
        let result = if filtered.is_empty() {
            None
        } else {
            Some(filtered)
        };
        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- selection range -----------------------------------------------------

    fn handle_selection_range(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, SelectionRangeParams) = match req
            .extract::<SelectionRangeParams>(SelectionRangeRequest::METHOD)
        {
            Ok(v) => v,
            Err(e) => {
                let resp = Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    format!("invalid selection-range params: {e:?}"),
                );
                let _ = self.connection.sender.send(Message::Response(resp));
                return;
            }
        };
        let uri = &params.text_document.uri;
        let Some(file_id) = self.file_id_for_uri(uri) else {
            let resp = Response::new_ok(id, Option::<Vec<SelectionRange>>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
        let analysis = self.host.analysis();
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let line_index = LineIndex::new(&source);

        let mut results = Vec::new();
        for pos in &params.positions {
            let offset = line_index.offset(
                pos.line as usize,
                pos.character as usize,
                &source,
            );
            let mut ranges = Vec::new();
            let parse = analysis.parse(file_id);
            let root = parse.syntax_node();
            // Walk up from token to root, collecting parent ranges.
            if let Some(token) = {
                let end: u32 = root.text_range().end().into();
                match root.token_at_offset((offset as u32).min(end).into()) {
                    rowan::TokenAtOffset::Single(t) => Some(t),
                    rowan::TokenAtOffset::Between(l, _) => Some(l),
                    _ => None,
                }
            } {
                let mut current = token.parent();
                while let Some(node) = current {
                    let rng = node.text_range();
                    let start = rng.start().into();
                    let end = rng.end().into();
                    if start < end {
                        let (sl, sc) = line_index.line_col(start, &source);
                        let (el, ec) = line_index.line_col(end, &source);
                        ranges.push(SelectionRange {
                            range: Range {
                                start: Position::new(sl as u32, sc as u32),
                                end: Position::new(el as u32, ec as u32),
                            },
                            parent: None,
                        });
                    }
                    current = node.parent();
                }
            }
            // Chain ranges as parent→child for the tree structure.
            let mut chain: Option<SelectionRange> = None;
            for r in ranges.into_iter().rev() {
                chain = Some(SelectionRange {
                    range: r.range,
                    parent: chain.map(Box::new),
                });
            }
            if let Some(root) = chain {
                results.push(root);
            }
        }
        let resp = Response::new_ok(id, results);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- code lens -----------------------------------------------------------

    fn handle_code_lens(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, CodeLensParams) =
            match req.extract::<CodeLensParams>(CodeLensRequest::METHOD) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid code-lens params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let Some(file_id) = self.file_id_for_uri(&params.text_document.uri) else {
            let resp = Response::new_ok(id, Option::<Vec<CodeLens>>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
        let analysis = self.host.analysis();
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let line_index = LineIndex::new(&source);
        let def_map = analysis.def_map(file_id);
        // Precompute: for each name, count how many function bodies
        // reference it (used by Function/Method CodeLens below).
        let mut ref_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let total_impl_count = def_map
            .definitions()
            .filter(|d| d.kind() == rua_analysis::DefKind::Impl)
            .count();
        for d in def_map.definitions() {
            if matches!(
                d.kind(),
                rua_analysis::DefKind::Function | rua_analysis::DefKind::Method
            ) {
                if let Some(body) = analysis.body(d.id()) {
                    let mut seen = std::collections::HashSet::new();
                    for (_, nr) in body.name_refs() {
                        if let Some(n) = nr.name() {
                            seen.insert(n.to_string());
                        }
                    }
                    for n in seen {
                        *ref_counts.entry(n).or_default() += 1;
                    }
                }
            }
        }

        let mut lenses = Vec::new();

        for definition in def_map.definitions() {
            if definition.file_id() != file_id {
                continue;
            }
            let kind = definition.kind();
            let title = match kind {
                rua_analysis::DefKind::Function | rua_analysis::DefKind::Method => {
                    let ref_count = ref_counts
                        .get(definition.name())
                        .copied()
                        .unwrap_or(0);
                    format!("{ref_count} reference(s)")
                }
                rua_analysis::DefKind::Struct => {
                    let impl_count = def_map
                        .definitions()
                        .filter(|d| {
                            d.kind() == rua_analysis::DefKind::Impl
                                && d.name() == definition.name()
                        })
                        .count();
                    if impl_count == 0 {
                        "struct".to_string()
                    } else {
                        format!("{impl_count} impl(s)")
                    }
                }
                rua_analysis::DefKind::Trait => {
                    if total_impl_count == 0 {
                        "trait".to_string()
                    } else {
                        format!("{total_impl_count} impl(s)")
                    }
                }
                _ => continue,
            };
            let name_range = definition.name_range();
            let start = name_range.start() as usize;
            let (line, col) = line_index.line_col(start, &source);
            lenses.push(CodeLens {
                range: Range {
                    start: Position::new(line as u32, col as u32),
                    end: Position::new(line as u32, col as u32),
                },
                command: None,
                data: None,
            });
            // Store title in command for VS Code
            if let Some(last) = lenses.last_mut() {
                last.command = Some(lsp_types::Command {
                    title,
                    command: String::new(),
                    arguments: None,
                });
            }
        }
        let resp = Response::new_ok(id, lenses);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- hover ---------------------------------------------------------------

    fn handle_hover(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params) = match req.extract::<lsp_types::HoverParams>(HoverRequest::METHOD) {
            Ok(v) => v,
            Err(e) => {
                let resp = Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    format!("invalid hover params: {e:?}"),
                );
                let _ = self.connection.sender.send(Message::Response(resp));
                return;
            }
        };
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let result = self
            .project_position(uri, pos)
            .and_then(|pp| {
                let analysis = self.host.analysis();
                analysis.hover(pp)
            })
            .map(|hover| to_lsp_hover(&hover));

        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- goto definition -----------------------------------------------------

    fn handle_definition(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params) =
            match req.extract::<lsp_types::GotoDefinitionParams>(GotoDefinition::METHOD) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid goto-def params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let result = self
            .project_position(uri, pos)
            .and_then(|pp| {
                let analysis = self.host.analysis();
                analysis.goto_definition(pp)
            })
            .and_then(|target| self.nav_to_location(&target));

        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- goto implementation -------------------------------------------------

    fn handle_goto_implementation(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params) = match req
            .extract::<lsp_types::GotoDefinitionParams>(GotoImplementation::METHOD)
        {
            Ok(v) => v,
            Err(e) => {
                let resp = Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    format!("invalid goto-impl params: {e:?}"),
                );
                let _ = self.connection.sender.send(Message::Response(resp));
                return;
            }
        };
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let locations: Vec<Location> = self
            .project_position(uri, pos)
            .map(|pp| {
                let analysis = self.host.analysis();
                let targets = analysis.goto_implementation(pp);
                targets
                    .into_iter()
                    .filter_map(|t| {
                        let file_range = t.target_range();
                        let uri = self.uri_for_file(file_range.file_id)?;
                        let range =
                            self.range_for_file(file_range.file_id, file_range.range)?;
                        Some(Location { uri, range })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let result = if locations.is_empty() {
            None
        } else {
            Some(GotoDefinitionResponse::Array(locations))
        };
        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- document symbols ----------------------------------------------------

    fn handle_document_symbol(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params) =
            match req.extract::<lsp_types::DocumentSymbolParams>(DocumentSymbolRequest::METHOD) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid doc-symbol params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let uri = &params.text_document.uri;
        let Some(file_id) = self.file_id_for_uri(uri) else {
            let resp = Response::new_ok(id, Option::<DocumentSymbolResponse>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };

        let analysis = self.host.analysis();
        let symbols = analysis.document_symbols(file_id, file_id);
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let line_index = LineIndex::new(&source);
        let nested = build_document_symbol_tree(&symbols, &line_index, &source);
        let result = DocumentSymbolResponse::Nested(nested);

        let resp = Response::new_ok(id, Some(result));
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- completion ----------------------------------------------------------

    fn handle_completion(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params) =
            match req.extract::<lsp_types::CompletionParams>(Completion::METHOD) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid completion params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;

        if self.file_id_for_uri(uri).is_none() {
            let resp = Response::new_ok(id, CompletionResponse::Array(Vec::new()));
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };

        let analysis = self.host.analysis();

        let items = self
            .project_position(uri, pos)
            .map(|pp| {
                let source = analysis.parse(pp.position.file_id).syntax_node().text().to_string();
                let line_index = LineIndex::new(&source);
                let native_items = analysis.completions(pp);
                let file_id = pp.position.file_id;
                native_items
                    .into_iter()
                    .map(|item| completion_to_lsp(&item, &line_index, &source, file_id))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // Use CompletionList with is_incomplete for large result sets so VS
        // Code re-queries as the user types more characters.
        const INCOMPLETE_THRESHOLD: usize = 100;
        let result = if items.len() > INCOMPLETE_THRESHOLD {
            CompletionResponse::List(CompletionList {
                is_incomplete: true,
                items,
            })
        } else {
            CompletionResponse::Array(items)
        };
        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- signature help ------------------------------------------------------

    fn handle_signature_help(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, lsp_types::SignatureHelpParams) =
            match req.extract::<lsp_types::SignatureHelpParams>(SignatureHelpRequest::METHOD) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid signature-help params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let result = self
            .project_position(uri, pos)
            .and_then(|pp| {
                let analysis = self.host.analysis();
                analysis.signature_help(pp)
            })
            .map(|info| {
                let parameters: Vec<ParameterInformation> = info
                    .parameters
                    .iter()
                    .map(|p| ParameterInformation {
                        label: ParameterLabel::Simple(p.clone()),
                        documentation: None,
                    })
                    .collect();
                SignatureHelp {
                    signatures: vec![SignatureInformation {
                        label: info.label,
                        documentation: None,
                        parameters: Some(parameters),
                        active_parameter: Some(info.active_parameter),
                    }],
                    active_signature: Some(0),
                    active_parameter: Some(info.active_parameter),
                }
            });

        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- inlay hints ---------------------------------------------------------

    fn handle_inlay_hint(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, InlayHintParams) =
            match req.extract::<InlayHintParams>(InlayHintRequest::METHOD) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid inlay-hint params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let uri = &params.text_document.uri;
        let Some(file_id) = self.file_id_for_uri(uri) else {
            let resp = Response::new_ok(id, Option::<Vec<InlayHint>>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };

        let analysis = self.host.analysis();
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let line_index = LineIndex::new(&source);

        let mut hints: Vec<InlayHint> = Vec::new();
        let def_map = analysis.def_map(file_id);

        for definition in def_map.definitions() {
            if !matches!(
                definition.kind(),
                rua_analysis::DefKind::Function | rua_analysis::DefKind::Method
            ) {
                continue;
            }
            let Some(body) = analysis.body(definition.id()) else { continue };
            let Some(source_map) = analysis.body_source_map(definition.id()) else {
                continue;
            };
            let Some(inference) = analysis.infer(definition.id()) else {
                continue;
            };

            // Type hints for let bindings: show `: Type` after the name.
            for (binding_id, binding) in body.bindings() {
                let Some(_name) = binding.name() else { continue };
                let Some(ty) = inference.type_of_binding(binding_id) else {
                    continue;
                };
                if ty.is_unknown() || ty.is_never() {
                    continue;
                }
                let Some(fr) = source_map.binding_range(binding_id) else {
                    continue;
                };
                let end = fr.range.end() as usize;
                if end > source.len() {
                    continue;
                }
                let (line, col) = line_index.line_col(end, &source);
                let ty_str = ty.to_string();
                // Make type hint clickable: if it's a named type, link to its definition.
                let label = if let rua_analysis::Ty::Named(named) = ty {
                    if let Some(def) = def_map.definition(named.definition()) {
                        let def_source = analysis
                            .parse(def.file_id())
                            .syntax_node()
                            .text()
                            .to_string();
                        let def_li = LineIndex::new(&def_source);
                        let def_start = def.name_range().start() as usize;
                        let (def_line, def_col) =
                            def_li.line_col(def_start, &def_source);
                        let def_uri = self
                            .uri_for_file(def.file_id())
                            .unwrap_or_else(|| uri.clone());
                        InlayHintLabel::LabelParts(vec![InlayHintLabelPart {
                            value: format!(": {ty_str}"),
                            tooltip: None,
                            location: Some(Location {
                                uri: def_uri,
                                range: Range {
                                    start: Position::new(def_line as u32, def_col as u32),
                                    end: Position::new(
                                        def_line as u32,
                                        (def_col + def.name().len()) as u32,
                                    ),
                                },
                            }),
                            command: None,
                        }])
                    } else {
                        InlayHintLabel::String(format!(": {ty_str}"))
                    }
                } else {
                    InlayHintLabel::String(format!(": {ty_str}"))
                };
                hints.push(InlayHint {
                    position: Position::new(line as u32, col as u32),
                    label,
                    kind: Some(InlayHintKind::TYPE),
                    padding_left: Some(true),
                    padding_right: None,
                    tooltip: None,
                    text_edits: None,
                    data: None,
                });
            }
        }

        hints.sort_by_key(|h| (h.position.line, h.position.character));
        let resp = Response::new_ok(id, hints);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- document highlight --------------------------------------------------

    fn handle_document_highlight(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, DocumentHighlightParams) =
            match req.extract::<DocumentHighlightParams>(DocumentHighlightRequest::METHOD) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid document-highlight params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let highlights = self
            .project_position(uri, pos)
            .map(|pp| {
                let analysis = self.host.analysis();
                // Reuse references logic to find all occurrences.
                let refs = analysis.references(pp, true);
                refs.into_iter()
                    .filter_map(|r| {
                        let file_range = r.range();
                        self.range_for_file(file_range.file_id, file_range.range)
                            .map(|range| DocumentHighlight {
                                range,
                                kind: match r.kind() {
                                    rua_analysis::ReferenceKind::Write => {
                                        Some(DocumentHighlightKind::WRITE)
                                    }
                                    rua_analysis::ReferenceKind::Read => {
                                        Some(DocumentHighlightKind::READ)
                                    }
                                    rua_analysis::ReferenceKind::Declaration => {
                                        Some(DocumentHighlightKind::TEXT)
                                    }
                                },
                            })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let resp = Response::new_ok(id, highlights);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- on-type formatting --------------------------------------------------

    fn handle_on_type_formatting(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, DocumentOnTypeFormattingParams) =
            match req.extract::<DocumentOnTypeFormattingParams>(OnTypeFormatting::METHOD) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid on-type-formatting params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        // Only handle Enter key.
        if params.ch != "\n" {
            let resp = Response::new_ok(id, Option::<Vec<TextEdit>>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        }

        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let Some(file_id) = self.file_id_for_uri(uri) else {
            let resp = Response::new_ok(id, Option::<Vec<TextEdit>>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };

        let analysis = self.host.analysis();
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let line_index = LineIndex::new(&source);

        let mut edits: Vec<TextEdit> = Vec::new();

        // Get the current line text up to the cursor.
        let line = pos.line as usize;
        let line_start = line_index.offset(line, 0, &source);
        let cursor_offset = line_index.offset(line, pos.character as usize, &source);
        let line_text = &source[line_start..cursor_offset.min(source.len())];
        let trimmed = line_text.trim_start();

        // Doc comment continuation: if the line starts with `///`, insert `/// `
        // on the next line.
        if trimmed.starts_with("///") {
            let after_slashes = trimmed.strip_prefix("///").unwrap_or("");
            let continuation = if after_slashes.trim().is_empty() {
                // Empty doc comment line — just `/// `.
                "/// ".to_string()
            } else {
                // Continue with `/// `.
                "/// ".to_string()
            };
            let edit_pos = Position::new(pos.line + 1, 0);
            let (line, col) = (
                edit_pos.line,
                0u32,
            );
            edits.push(TextEdit {
                range: Range {
                    start: Position::new(line, col),
                    end: Position::new(line, col),
                },
                new_text: continuation,
            });
        }

        // Auto-indent after `{`: add indentation on the new line.
        if line_text.trim_end().ends_with('{') {
            let leading_ws: String = line_text
                .chars()
                .take_while(|c| c.is_whitespace())
                .collect();
            let indent = format!("{leading_ws}    ");
            let edit_pos = Position::new(pos.line + 1, 0);
            edits.push(TextEdit {
                range: Range {
                    start: Position::new(edit_pos.line, 0),
                    end: Position::new(edit_pos.line, 0),
                },
                new_text: indent,
            });
        }

        let result = if edits.is_empty() {
            None
        } else {
            Some(edits)
        };
        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- code actions --------------------------------------------------------

    #[allow(clippy::mutable_key_type)]
    fn handle_code_action(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, CodeActionParams) =
            match req.extract::<CodeActionParams>(CodeActionRequest::METHOD) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid code-action params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let uri = &params.text_document.uri;
        let Some(file_id) = self.file_id_for_uri(uri) else {
            let resp = Response::new_ok(id, Option::<CodeActionResponse>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };

        let analysis = self.host.analysis();
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let line_index = LineIndex::new(&source);

        // Get the offset of the code action range start.
        let start_offset = line_index.offset(
            params.range.start.line as usize,
            params.range.start.character as usize,
            &source,
        );
        let end_offset = line_index.offset(
            params.range.end.line as usize,
            params.range.end.character as usize,
            &source,
        );

        let mut actions: Vec<CodeAction> = Vec::new();

        // Diagnostic-based quick fixes.
        for diag in &params.context.diagnostics {
            if let Some(lsp_types::NumberOrString::String(code_str)) = diag.code.as_ref() {
                match code_str.as_str() {
                    "W0300" => {
                        // Unused variable → prefix with _
                        let diag_range = &diag.range;
                        let d_start = line_index.offset(
                            diag_range.start.line as usize,
                            diag_range.start.character as usize,
                            &source,
                        );
                        // Get the variable name from source at the diagnostic range.
                        let d_end = line_index.offset(
                            diag_range.end.line as usize,
                            diag_range.end.character as usize,
                            &source,
                        );
                        if d_end > d_start && d_end <= source.len() {
                            let var_name = source[d_start..d_end].to_string();
                            let edit = TextEdit {
                                range: *diag_range,
                                new_text: format!("_{var_name}"),
                            };
                            let mut changes = std::collections::HashMap::new();
                            changes.insert(uri.clone(), vec![edit]);
                            actions.push(CodeAction {
                                title: format!("Rename to `_{var_name}` (suppress warning)"),
                                kind: Some(CodeActionKind::QUICKFIX),
                                diagnostics: Some(vec![diag.clone()]),
                                edit: Some(WorkspaceEdit {
                                    changes: Some(changes),
                                    ..Default::default()
                                }),
                                command: None,
                                is_preferred: Some(true),
                                disabled: None,
                                data: None,
                            });
                        }
                    }
                    "E0206" => {
                        // Immutable assignment → add `mut`
                        let diag_range = &diag.range;
                        let (sl, sc) = (
                            diag_range.start.line,
                            diag_range.start.character,
                        );
                        // Find the `let` keyword by scanning backwards for the
                        // word `let` at a word boundary (not inside an identifier
                        // like `outlet_name` or a string literal).
                        let d_start = line_index.offset(sl as usize, sc as usize, &source);
                        let before = source[..d_start]
                            .rmatch_indices("let ")
                            .find(|&(pos, _)| {
                                pos == 0
                                    || !rua_syntax::text::is_ident_byte(
                                        source.as_bytes()[pos - 1],
                                    )
                            })
                            .map(|(pos, _)| pos)
                            .unwrap_or(d_start);
                        let insert_pos = before + 4; // after "let "
                        let (il, ic) = line_index.line_col(insert_pos, &source);
                        let edit = TextEdit {
                            range: Range {
                                start: Position::new(il as u32, ic as u32),
                                end: Position::new(il as u32, ic as u32),
                            },
                            new_text: "mut ".to_string(),
                        };
                        let mut changes = std::collections::HashMap::new();
                        changes.insert(uri.clone(), vec![edit]);
                        actions.push(CodeAction {
                            title: "Add `mut` to variable".to_string(),
                            kind: Some(CodeActionKind::QUICKFIX),
                            diagnostics: Some(vec![diag.clone()]),
                            edit: Some(WorkspaceEdit {
                                changes: Some(changes),
                                ..Default::default()
                            }),
                            command: None,
                            is_preferred: Some(true),
                            disabled: None,
                            data: None,
                        });
                    }
                    _ => {}
                }
            }
        }

        // "Fill match arms" — find match expressions at the cursor.
        let def_map = analysis.def_map(file_id);
        for definition in def_map.definitions() {
            if !matches!(
                definition.kind(),
                rua_analysis::DefKind::Function | rua_analysis::DefKind::Method
            ) {
                continue;
            }
            let Some(body) = analysis.body(definition.id()) else { continue };
            let Some(source_map) = analysis.body_source_map(definition.id()) else {
                continue;
            };
            let Some(inference) = analysis.infer(definition.id()) else {
                continue;
            };

            for (expr_id, expr) in body.exprs() {
                let rua_analysis::Expr::Match { scrutinee, arms } = expr else {
                    continue;
                };
                let Some(expr_range) = source_map.expr_range(expr_id) else {
                    continue;
                };
                let m_start = expr_range.range.start() as usize;
                let m_end = expr_range.range.end() as usize;
                // Only if overlap.
                if start_offset > m_end || end_offset < m_start {
                    continue;
                }

                let scrutinee_ty = inference.type_of_expr(*scrutinee).cloned();
                let Some(scrutinee_ty) = scrutinee_ty else { continue };

                // Check if enum
                let member_index = analysis.member_index(file_id);
                let Some(enum_template) = (|| {
                    let named = match &scrutinee_ty {
                        rua_analysis::Ty::Named(n) => n,
                        _ => return None,
                    };
                    let d = def_map.definition(named.definition())?;
                    if d.kind() != rua_analysis::DefKind::Enum {
                        return None;
                    }
                    member_index.type_template(d.id()).cloned()
                })() else {
                    continue;
                };

                let all_variants =
                    member_index.associated_candidates(&enum_template);
                // Collect existing arm names from pattern paths.
                let existing_names: Vec<String> = arms
                    .iter()
                    .filter_map(|arm| {
                        let pat_id = arm.patterns().first()?;
                        let pat = body.pattern(*pat_id)?;
                        let path = match pat {
                            rua_analysis::Pat::Path(p)
                            | rua_analysis::Pat::TupleVariant { path: p, .. }
                            | rua_analysis::Pat::StructVariant { path: p, .. } => p,
                            _ => return None,
                        };
                        let last_seg = path.last()?;
                        let name_ref = body.name_ref(*last_seg)?;
                        name_ref.name().map(|n| n.to_string())
                    })
                    .collect();

                let mut new_arms = String::new();
                for variant in &all_variants {
                    let name = variant.name();
                    if existing_names.iter().any(|n| n == name) {
                        continue;
                    }
                    let arm_text = if variant.ty().to_string() == "()" {
                        format!("        {name} => todo!(),\n")
                    } else {
                        format!("        {name}(..) => todo!(),\n")
                    };
                    new_arms.push_str(&arm_text);
                }
                if new_arms.is_empty() {
                    continue;
                }

                // Find the `{` of the match body and insert after newline.
                let lbrace = source[..m_end].rfind('{').unwrap_or(m_start);
                let newline_off = source[lbrace..].find('\n').map(|n| lbrace + n + 1);
                let insert_pos = newline_off.unwrap_or(lbrace + 1);

                let (line, col) = line_index.line_col(insert_pos, &source);
                let edit = TextEdit {
                    range: Range {
                        start: Position::new(line as u32, col as u32),
                        end: Position::new(line as u32, col as u32),
                    },
                    new_text: format!("\n{new_arms}"),
                };

                let mut changes = std::collections::HashMap::new();
                changes.insert(uri.clone(), vec![edit]);
                let workspace_edit = WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                };

                actions.push(CodeAction {
                    title: format!(
                        "Fill match arms ({} missing)",
                        all_variants.len() - existing_names.len()
                    ),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: None,
                    edit: Some(workspace_edit),
                    command: None,
                    is_preferred: None,
                    disabled: None,
                    data: None,
                });
                // Only one fill action per request
                break;
            }
        }

        // "Generate impl members" — for empty `impl Trait for Type {}` blocks.
        for definition in def_map.definitions() {
            if definition.kind() != rua_analysis::DefKind::Impl {
                continue;
            }
            let impl_range = definition.range();
            let i_start = impl_range.start() as usize;
            let i_end = impl_range.end() as usize;
            if start_offset > i_end || end_offset < i_start {
                continue;
            }
            let sig = definition.signature();
            let trait_name = match sig {
                rua_analysis::ItemSignature::Impl(s) => s
                    .trait_ref()
                    .as_ref()
                    .and_then(|tr| tr.syntax().map(|s| s.to_string())),
                _ => continue,
            };
            let Some(trait_name) = trait_name else { continue };
            // Find the trait definition by name.
            let trait_def = def_map.definitions().find(|d| {
                d.kind() == rua_analysis::DefKind::Trait && d.name() == trait_name
            });
            let Some(trait_def) = trait_def else { continue };
            let existing_names: Vec<&str> = def_map
                .members(definition.id())
                .map(|d| d.name())
                .collect();
            let mut new_methods = String::new();
            for method in def_map.members(trait_def.id()) {
                if existing_names.contains(&method.name()) {
                    continue;
                }
                let sig = method.signature();
                let sig_str = match sig {
                    rua_analysis::ItemSignature::Callable(cs) => {
                        let params: Vec<String> = cs
                            .params()
                            .iter()
                            .filter_map(|p| {
                                let name = p.name()?;
                                let ty = p.type_ref().syntax()?;
                                Some(format!("{name}: {ty}"))
                            })
                            .collect();
                        let ret = cs.return_type().syntax().unwrap_or("()");
                        format!("fn {}({}) -> {ret}", method.name(), params.join(", "))
                    }
                    _ => format!("fn {}()", method.name()),
                };
                new_methods.push_str(&format!("    {sig_str} {{ todo!() }}\n"));
            }
            if new_methods.is_empty() {
                continue;
            }
            let lbrace = source[i_start..i_end].find('{').unwrap_or(0) + i_start;
            let insert_pos = source[lbrace..]
                .find('\n')
                .map(|n| lbrace + n + 1)
                .unwrap_or(lbrace + 1);
            let (line, col) = line_index.line_col(insert_pos, &source);
            let edit = TextEdit {
                range: Range {
                    start: Position::new(line as u32, col as u32),
                    end: Position::new(line as u32, col as u32),
                },
                new_text: format!("\n{new_methods}"),
            };
            let mut changes = std::collections::HashMap::new();
            changes.insert(uri.clone(), vec![edit]);
            actions.push(CodeAction {
                title: format!(
                    "Generate impl members ({} missing)",
                    def_map.members(trait_def.id()).count() - existing_names.len()
                ),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: None,
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                }),
                command: None,
                is_preferred: None,
                disabled: None,
                data: None,
            });
            break;
        }

        // "Extract variable" — when the user has a range selected.
        let sel_start = start_offset;
        let sel_end = end_offset;
        if sel_end > sel_start
            && (sel_end - sel_start) > 1
        {
            let sel_text = source[sel_start..sel_end.min(source.len())].trim().to_string();
            if !sel_text.is_empty() && sel_text.len() < 200 {
                // Generate a variable name from the text (lowercase first letter
                // of the expression type hint, or just "var").
                let var_name = "var_name"; // simple default
                let insert_line = {
                    let (l, _) = line_index.line_col(sel_start, &source);
                    l
                };
                let leading_ws: String = source[..sel_start]
                    .chars()
                    .rev()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                let edit1 = TextEdit {
                    range: Range {
                        start: Position::new(insert_line as u32, 0),
                        end: Position::new(insert_line as u32, 0),
                    },
                    new_text: format!("{leading_ws}let {var_name} = {sel_text};\n"),
                };
                let (el, ec) = line_index.line_col(sel_end.min(source.len()), &source);
                let edit2 = TextEdit {
                    range: Range {
                        start: Position::new(insert_line as u32, leading_ws.len() as u32),
                        end: Position::new(el as u32, ec as u32),
                    },
                    new_text: var_name.to_string(),
                };
                let mut changes = std::collections::HashMap::new();
                changes.insert(uri.clone(), vec![edit1, edit2]);
                actions.push(CodeAction {
                    title: format!("Extract to variable `{var_name}`"),
                    kind: Some(CodeActionKind::REFACTOR_EXTRACT),
                    diagnostics: None,
                    edit: Some(WorkspaceEdit {
                        changes: Some(changes),
                        ..Default::default()
                    }),
                    command: None,
                    is_preferred: None,
                    disabled: None,
                    data: None,
                });
            }

            // "Wrap in block" — wrap selected code in { }
            if !sel_text.contains('\n') {
                let (sl, sc) = line_index.line_col(sel_start, &source);
                let edit = TextEdit {
                    range: Range {
                        start: Position::new(sl as u32, sc as u32),
                        end: {
                            let (el, ec) =
                                line_index.line_col(sel_end.min(source.len()), &source);
                            Position::new(el as u32, ec as u32)
                        },
                    },
                    new_text: format!("{{\n    {sel_text}\n}}"),
                };
                let mut changes = std::collections::HashMap::new();
                changes.insert(uri.clone(), vec![edit]);
                actions.push(CodeAction {
                    title: "Wrap in block".to_string(),
                    kind: Some(CodeActionKind::REFACTOR_REWRITE),
                    diagnostics: None,
                    edit: Some(WorkspaceEdit {
                        changes: Some(changes),
                        ..Default::default()
                    }),
                    command: None,
                    is_preferred: None,
                    disabled: None,
                    data: None,
                });
            }
        }

        // The actions below use the cursor position (sel_start) but do
        // NOT require a text selection — they work on the enclosing
        // struct, function, or statement at the cursor.

        // "Sort struct fields" — alphabetically reorder fields.
            for definition in def_map.definitions() {
                if definition.kind() != rua_analysis::DefKind::Struct {
                    continue;
                }
                let sr = definition.range();
                if start_offset as u32 > sr.end() || (end_offset as u32) < sr.start() {
                    continue;
                }
                let mut fields: Vec<(&str, rua_analysis::TextRange)> = def_map
                    .members(definition.id())
                    .filter(|d| d.kind() == rua_analysis::DefKind::Field)
                    .map(|d| (d.name(), d.name_range()))
                    .collect();
                if fields.len() < 2 {
                    continue;
                }
                let original = fields.clone();
                fields.sort_by_key(|(name, _)| *name);
                if original.iter().map(|(n, _)| *n).eq(fields.iter().map(|(n, _)| *n)) {
                    continue; // already sorted
                }
                let mut edits = Vec::new();
                for (i, (_name, range)) in original.iter().enumerate() {
                    let new_text = fields[i].0.to_string();
                    let (sl, sc) = line_index.line_col(range.start() as usize, &source);
                    let (el, ec) = line_index.line_col(range.end() as usize, &source);
                    edits.push(TextEdit {
                        range: Range {
                            start: Position::new(sl as u32, sc as u32),
                            end: Position::new(el as u32, ec as u32),
                        },
                        new_text,
                    });
                }
                let mut changes = std::collections::HashMap::new();
                changes.insert(uri.clone(), edits);
                actions.push(CodeAction {
                    title: "Sort struct fields".to_string(),
                    kind: Some(CodeActionKind::REFACTOR_REWRITE),
                    diagnostics: None,
                    edit: Some(WorkspaceEdit {
                        changes: Some(changes),
                        ..Default::default()
                    }),
                    command: None,
                    is_preferred: None,
                    disabled: None,
                    data: None,
                });
                break;
            }

            // "Remove trailing comma" — if cursor is on a comma before )/]/}.
            let comma_offset = line_index.offset(
                params.range.start.line as usize,
                params.range.start.character as usize,
                &source,
            );
            if comma_offset < source.len()
                && source.as_bytes().get(comma_offset) == Some(&b',')
            {
                let after: String = source[comma_offset + 1..]
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .collect();
                let next = source[comma_offset + 1 + after.len()..]
                    .chars()
                    .next();
                if next.is_some_and(|c| c == ')' || c == ']' || c == '}') {
                    let (sl, sc) = line_index.line_col(comma_offset, &source);
                    let edit = TextEdit {
                        range: Range {
                            start: Position::new(sl as u32, sc as u32),
                            end: Position::new(sl as u32, sc as u32 + 1),
                        },
                        new_text: String::new(),
                    };
                    let mut changes = std::collections::HashMap::new();
                    changes.insert(uri.clone(), vec![edit]);
                    actions.push(CodeAction {
                        title: "Remove trailing comma".to_string(),
                        kind: Some(CodeActionKind::REFACTOR_REWRITE),
                        diagnostics: None,
                        edit: Some(WorkspaceEdit {
                            changes: Some(changes),
                            ..Default::default()
                        }),
                        command: None,
                        is_preferred: None,
                        disabled: None,
                        data: None,
                    });
                }
            }

        // "Extract function" — wrap selected code in a new function.
        if sel_end > sel_start && (sel_end - sel_start) > 10 {
            let sel_text =
                source[sel_start..sel_end.min(source.len())].to_string();
            if sel_text.contains('\n') && !sel_text.trim().is_empty() {
                let func_name = "extracted";
                let (sl, _sc) = line_index.line_col(sel_start, &source);
                // Build the extracted function and replace selection with call.
                let new_func = format!(
                    "fn {func_name}() {{\n{sel_text}\n}}\n\n"
                );
                let call_text = format!("{func_name}();");
                let edit1 = TextEdit {
                    range: Range {
                        start: Position::new(0, 0),
                        end: Position::new(0, 0),
                    },
                    new_text: new_func,
                };
                let (el, ec) =
                    line_index.line_col(sel_end.min(source.len()), &source);
                let edit2 = TextEdit {
                    range: Range {
                        start: Position::new(sl as u32, 0),
                        end: Position::new(el as u32, ec as u32),
                    },
                    new_text: format!("    {call_text}"),
                };
                let mut changes = std::collections::HashMap::new();
                changes.insert(uri.clone(), vec![edit1, edit2]);
                actions.push(CodeAction {
                    title: format!("Extract to function `{func_name}`"),
                    kind: Some(CodeActionKind::REFACTOR_EXTRACT),
                    diagnostics: None,
                    edit: Some(WorkspaceEdit {
                        changes: Some(changes),
                        ..Default::default()
                    }),
                    command: None,
                    is_preferred: None,
                    disabled: None,
                    data: None,
                });
            }
        }

        // "Inline function" — replace a function call with its body.
        for definition in def_map.definitions() {
            if !matches!(
                definition.kind(),
                rua_analysis::DefKind::Function | rua_analysis::DefKind::Method
            ) {
                continue;
            }
            let Some(body) = analysis.body(definition.id()) else { continue };
            let Some(source_map) = analysis.body_source_map(definition.id()) else {
                continue;
            };
            for (eid, expr) in body.exprs() {
                let rua_analysis::Expr::Call { callee, args: _ } = expr else {
                    continue;
                };
                let Some(fr) = source_map.expr_range(eid) else { continue };
                let es = fr.range.start() as usize;
                let ee = fr.range.end() as usize;
                if start_offset > ee || end_offset < es {
                    continue;
                }
                // Find the called function's body.
                let callee_expr = body.expr(*callee);
                let callee_name = match callee_expr {
                    Some(rua_analysis::Expr::Path(path)) => path
                        .last()
                        .and_then(|nrid| body.name_ref(*nrid))
                        .and_then(|nr| nr.name()),
                    _ => None,
                };
                let Some(callee_name) = callee_name else { continue };
                let callee_def = def_map
                    .definitions()
                    .find(|d| d.name() == callee_name && d.kind() == rua_analysis::DefKind::Function);
                let Some(callee_def) = callee_def else { continue };
                let Some(callee_body) = analysis.body(callee_def.id()) else {
                    continue;
                };
                let Some(callee_sm) = analysis.body_source_map(callee_def.id()) else {
                    continue;
                };
                // Get body text.
                let body_exprs: Vec<(rua_analysis::ExprId, &rua_analysis::Expr)> =
                    callee_body.exprs().collect();
                let body_text = if let Some(&(first_id, _)) = body_exprs.first() {
                    let last_id = body_exprs.last().map(|&(id, _)| id).unwrap_or(first_id);
                    let body_start = callee_sm
                        .expr_range(first_id)
                        .map(|r| r.range.start() as usize)
                        .unwrap_or(0);
                    let body_end = callee_sm
                        .expr_range(last_id)
                        .map(|r| r.range.end() as usize)
                        .unwrap_or(0);
                    source[body_start..body_end.min(source.len())].to_string()
                } else {
                    "{}".to_string()
                };
                // Replace call with body.
                let (sl, sc) = line_index.line_col(es, &source);
                let (el, ec) =
                    line_index.line_col(ee.min(source.len()), &source);
                let edit = TextEdit {
                    range: Range {
                        start: Position::new(sl as u32, sc as u32),
                        end: Position::new(el as u32, ec as u32),
                    },
                    new_text: body_text,
                };
                let mut changes = std::collections::HashMap::new();
                changes.insert(uri.clone(), vec![edit]);
                actions.push(CodeAction {
                    title: format!("Inline function `{callee_name}`"),
                    kind: Some(CodeActionKind::REFACTOR_INLINE),
                    diagnostics: None,
                    edit: Some(WorkspaceEdit {
                        changes: Some(changes),
                        ..Default::default()
                    }),
                    command: None,
                    is_preferred: None,
                    disabled: None,
                    data: None,
                });
                break;
            }
            if !actions.is_empty() {
                break;
            }
        }

        // "Replace if-let with match"
        for definition in def_map.definitions() {
            if !matches!(
                definition.kind(),
                rua_analysis::DefKind::Function | rua_analysis::DefKind::Method
            ) {
                continue;
            }
            let Some(body) = analysis.body(definition.id()) else { continue };
            let Some(source_map) = analysis.body_source_map(definition.id()) else {
                continue;
            };
            for (expr_id, expr) in body.exprs() {
                let rua_analysis::Expr::If {
                    condition:
                        rua_analysis::Condition::Let {
                            pattern: _,
                            scrutinee,
                        },
                    then_branch: then_body,
                    else_branch: else_body,
                } = expr
                else {
                    continue;
                };
                let Some(expr_range) = source_map.expr_range(expr_id) else {
                    continue;
                };
                let e_start = expr_range.range.start() as usize;
                let e_end = expr_range.range.end() as usize;
                if start_offset > e_end || end_offset < e_start {
                    continue;
                }
                // Get source text of the scrutinee and arms.
                let Some(scr_range) = source_map.expr_range(*scrutinee) else {
                    continue;
                };
                let scr_text =
                    source[scr_range.range.start() as usize..scr_range.range.end() as usize]
                        .to_string();
                let Some(then_range) = source_map.expr_range(*then_body) else {
                    continue;
                };
                let then_text =
                    source[then_range.range.start() as usize..then_range.range.end() as usize]
                        .to_string();
                let else_text = else_body
                    .as_ref()
                    .and_then(|eid| {
                        let r = source_map.expr_range(*eid)?;
                        Some(source[r.range.start() as usize..r.range.end() as usize].to_string())
                    })
                    .unwrap_or_else(|| "()".to_string());
                let new_text = format!(
                    "match {scr_text} {{\n        {then_text}\n        _ => {else_text},\n    }}"
                );
                let (sl, sc) = line_index.line_col(e_start, &source);
                let (el, ec) = line_index.line_col(e_end.min(source.len()), &source);
                let edit = TextEdit {
                    range: Range {
                        start: Position::new(sl as u32, sc as u32),
                        end: Position::new(el as u32, ec as u32),
                    },
                    new_text,
                };
                let mut changes = std::collections::HashMap::new();
                changes.insert(uri.clone(), vec![edit]);
                actions.push(CodeAction {
                    title: "Replace if-let with match".to_string(),
                    kind: Some(CodeActionKind::REFACTOR_REWRITE),
                    diagnostics: None,
                    edit: Some(WorkspaceEdit {
                        changes: Some(changes),
                        ..Default::default()
                    }),
                    command: None,
                    is_preferred: None,
                    disabled: None,
                    data: None,
                });
                break;
            }
            if !actions.is_empty() {
                break;
            }
        }

        // "Inline variable" — when cursor is on a variable usage.
        for definition in def_map.definitions() {
            if !matches!(
                definition.kind(),
                rua_analysis::DefKind::Function | rua_analysis::DefKind::Method
            ) {
                continue;
            }
            let Some(body) = analysis.body(definition.id()) else { continue };
            let Some(source_map) = analysis.body_source_map(definition.id()) else {
                continue;
            };
            let Some(resolution) = analysis.body_resolution(definition.id()) else {
                continue;
            };
            // Find the local binding at the cursor.
            for (name_ref_id, _nr) in body.name_refs() {
                let Some(fr) = source_map.name_ref_range(name_ref_id) else {
                    continue;
                };
                let nr_start = fr.range.start() as usize;
                let nr_end = fr.range.end() as usize;
                if nr_start < start_offset || nr_end > end_offset {
                    continue;
                }
                let Some(resolved) = resolution.resolve(name_ref_id) else {
                    continue;
                };
                let binding_id = match resolved {
                    rua_analysis::LocalResolveResult::Resolved(lid) => lid.binding(),
                    _ => continue,
                };
                let Some(binding) = body.binding(binding_id) else {
                    continue;
                };
                if binding.name().is_none_or(|n| n == "_") {
                    continue;
                }
                // Find the binding's definition expression.
                let binding_range = source_map.binding_range(binding_id);
                let def_expr = body
                    .exprs()
                    .find(|(eid, _)| {
                        source_map
                            .expr_range(*eid)
                            .is_some_and(|r| {
                                binding_range
                                    .is_some_and(|br| r.range.contains(br.range.start()))
                            })
                    });
                let Some((def_expr_id, _)) = def_expr else {
                    continue;
                };
                let Some(def_fr) = source_map.expr_range(def_expr_id) else {
                    continue;
                };
                let def_text = source
                    [def_fr.range.start() as usize..def_fr.range.end() as usize]
                    .to_string();
                // Replace the variable usage with the definition expression.
                let (sl, sc) = line_index.line_col(nr_start, &source);
                let (el, ec) = line_index.line_col(nr_end.min(source.len()), &source);
                let edit = TextEdit {
                    range: Range {
                        start: Position::new(sl as u32, sc as u32),
                        end: Position::new(el as u32, ec as u32),
                    },
                    new_text: def_text,
                };
                let mut changes = std::collections::HashMap::new();
                changes.insert(uri.clone(), vec![edit]);
                actions.push(CodeAction {
                    title: "Inline variable".to_string(),
                    kind: Some(CodeActionKind::REFACTOR_INLINE),
                    diagnostics: None,
                    edit: Some(WorkspaceEdit {
                        changes: Some(changes),
                        ..Default::default()
                    }),
                    command: None,
                    is_preferred: None,
                    disabled: None,
                    data: None,
                });
                break;
            }
            if !actions.is_empty() {
                break;
            }
        }

        let resp = Response::new_ok(id, actions);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- call hierarchy ------------------------------------------------------

    fn handle_call_hierarchy_prepare(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, lsp_types::CallHierarchyPrepareParams) =
            match req.extract::<lsp_types::CallHierarchyPrepareParams>(
                CallHierarchyPrepare::METHOD,
            ) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid call-hierarchy-prepare params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let result = self.project_position(uri, pos).and_then(|pp| {
            let analysis = self.host.analysis();
            analysis
                .call_hierarchy_prepare(pp)
                .map(|item| {
                    let (sl, sc) = {
                        let source =
                            analysis.parse(item.file_id).syntax_node().text().to_string();
                        let li = LineIndex::new(&source);
                        li.line_col(item.range.start() as usize, &source)
                    };
                    let (el, ec) = {
                        let source =
                            analysis.parse(item.file_id).syntax_node().text().to_string();
                        let li = LineIndex::new(&source);
                        li.line_col(item.range.end() as usize, &source)
                    };
                    vec![lsp_types::CallHierarchyItem { detail: None,
                        name: item.name,
                        kind: lsp_types::SymbolKind::FUNCTION,
                        uri: self.uri_for_file(item.file_id).unwrap_or_else(|| fallback_uri(item.file_id)),
                        range: Range {
                            start: Position::new(sl as u32, sc as u32),
                            end: Position::new(el as u32, ec as u32),
                        },
                        selection_range: Range {
                            start: Position::new(sl as u32, sc as u32),
                            end: Position::new(el as u32, ec as u32),
                        },
                        data: None,
                        tags: None,
                    }]
                })
        });
        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    fn handle_call_hierarchy_incoming(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, lsp_types::CallHierarchyIncomingCallsParams) =
            match req.extract::<lsp_types::CallHierarchyIncomingCallsParams>(
                CallHierarchyIncomingCalls::METHOD,
            ) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid call-hierarchy-incoming params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let result: Vec<lsp_types::CallHierarchyIncomingCall> = self
            .file_id_for_uri(&params.item.uri)
            .map(|file_id| {
                let analysis = self.host.analysis();
                let chi = rua_analysis::CallHierarchyItem {
                    name: params.item.name.clone(),
                    kind: rua_analysis::DefKind::Function,
                    file_id,
                    range: rua_analysis::TextRange::new(0, 0),
                };
                analysis
                    .call_hierarchy_incoming(&chi)
                    .into_iter()
                    .map(|item| {
                        let source =
                            analysis.parse(item.file_id).syntax_node().text().to_string();
                        let li = LineIndex::new(&source);
                        let (sl, sc) = li.line_col(item.range.start() as usize, &source);
                        let (el, ec) = li.line_col(item.range.end() as usize, &source);
                        lsp_types::CallHierarchyIncomingCall {
                            from: lsp_types::CallHierarchyItem { detail: None,
                                name: item.name,
                                kind: lsp_types::SymbolKind::FUNCTION,
                                uri: self
                                    .uri_for_file(item.file_id)
                                    .unwrap_or_else(|| fallback_uri(item.file_id)),
                                range: Range {
                                    start: Position::new(sl as u32, sc as u32),
                                    end: Position::new(el as u32, ec as u32),
                                },
                                selection_range: Range {
                                    start: Position::new(sl as u32, sc as u32),
                                    end: Position::new(el as u32, ec as u32),
                                },
                                data: None,
                                tags: None,
                            },
                            from_ranges: vec![Range {
                                start: Position::new(sl as u32, sc as u32),
                                end: Position::new(el as u32, ec as u32),
                            }],
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    fn handle_call_hierarchy_outgoing(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, lsp_types::CallHierarchyOutgoingCallsParams) =
            match req.extract::<lsp_types::CallHierarchyOutgoingCallsParams>(
                CallHierarchyOutgoingCalls::METHOD,
            ) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid call-hierarchy-outgoing params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let result: Vec<lsp_types::CallHierarchyOutgoingCall> = self
            .file_id_for_uri(&params.item.uri)
            .map(|file_id| {
                let analysis = self.host.analysis();
                let chi = rua_analysis::CallHierarchyItem {
                    name: params.item.name.clone(),
                    kind: rua_analysis::DefKind::Function,
                    file_id,
                    range: rua_analysis::TextRange::new(0, 0),
                };
                analysis
                    .call_hierarchy_outgoing(&chi)
                    .into_iter()
                    .map(|item| {
                        let source =
                            analysis.parse(item.file_id).syntax_node().text().to_string();
                        let li = LineIndex::new(&source);
                        let (sl, sc) = li.line_col(item.range.start() as usize, &source);
                        let (el, ec) = li.line_col(item.range.end() as usize, &source);
                        lsp_types::CallHierarchyOutgoingCall {
                            to: lsp_types::CallHierarchyItem { detail: None,
                                name: item.name,
                                kind: lsp_types::SymbolKind::FUNCTION,
                                uri: self
                                    .uri_for_file(item.file_id)
                                    .unwrap_or_else(|| fallback_uri(item.file_id)),
                                range: Range {
                                    start: Position::new(sl as u32, sc as u32),
                                    end: Position::new(el as u32, ec as u32),
                                },
                                selection_range: Range {
                                    start: Position::new(sl as u32, sc as u32),
                                    end: Position::new(el as u32, ec as u32),
                                },
                                data: None,
                                tags: None,
                            },
                            from_ranges: vec![Range {
                                start: Position::new(sl as u32, sc as u32),
                                end: Position::new(el as u32, ec as u32),
                            }],
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- type hierarchy ------------------------------------------------------

    fn handle_type_hierarchy_prepare(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, lsp_types::TypeHierarchyPrepareParams) =
            match req.extract::<lsp_types::TypeHierarchyPrepareParams>(
                TypeHierarchyPrepare::METHOD,
            ) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid type-hierarchy-prepare params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let result = self.project_position(uri, pos).and_then(|pp| {
            let analysis = self.host.analysis();
            analysis
                .type_hierarchy_prepare(pp)
                .map(|item| {
                    let source = analysis.parse(item.file_id).syntax_node().text().to_string();
                    let li = LineIndex::new(&source);
                    let (sl, sc) = li.line_col(item.range.start() as usize, &source);
                    let (el, ec) = li.line_col(item.range.end() as usize, &source);
                    vec![lsp_types::TypeHierarchyItem { detail: None,
                        name: item.name,
                        kind: lsp_types::SymbolKind::STRUCT,
                        uri: self.uri_for_file(item.file_id).unwrap_or_else(|| fallback_uri(item.file_id)),
                        range: Range {
                            start: Position::new(sl as u32, sc as u32),
                            end: Position::new(el as u32, ec as u32),
                        },
                        selection_range: Range {
                            start: Position::new(sl as u32, sc as u32),
                            end: Position::new(el as u32, ec as u32),
                        },
                        data: None,
                        tags: None,
                    }]
                })
        });
        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    fn handle_type_hierarchy_subtypes(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, lsp_types::TypeHierarchySubtypesParams) =
            match req.extract::<lsp_types::TypeHierarchySubtypesParams>(
                TypeHierarchySubtypes::METHOD,
            ) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid type-hierarchy-subtypes params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let result: Vec<lsp_types::TypeHierarchyItem> = self
            .file_id_for_uri(&params.item.uri)
            .map(|file_id| {
                let analysis = self.host.analysis();
                let thi = rua_analysis::TypeHierarchyItem {
                    name: params.item.name.clone(),
                    kind: rua_analysis::DefKind::Trait,
                    file_id,
                    range: rua_analysis::TextRange::new(0, 0),
                };
                analysis
                    .type_hierarchy_subtypes(&thi)
                    .into_iter()
                    .map(|item| {
                        let source =
                            analysis.parse(item.file_id).syntax_node().text().to_string();
                        let li = LineIndex::new(&source);
                        let (sl, sc) = li.line_col(item.range.start() as usize, &source);
                        let (el, ec) = li.line_col(item.range.end() as usize, &source);
                        lsp_types::TypeHierarchyItem { detail: None,
                            name: item.name,
                            kind: lsp_types::SymbolKind::STRUCT,
                            uri: self
                                .uri_for_file(item.file_id)
                                .unwrap_or_else(|| {
                                    format!("file:///unknown/{}", item.file_id.index())
                                        .parse()
                                        .unwrap()
                                }),
                            range: Range {
                                start: Position::new(sl as u32, sc as u32),
                                end: Position::new(el as u32, ec as u32),
                            },
                            selection_range: Range {
                                start: Position::new(sl as u32, sc as u32),
                                end: Position::new(el as u32, ec as u32),
                            },
                            data: None,
                            tags: None,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    fn handle_type_hierarchy_supertypes(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, lsp_types::TypeHierarchySupertypesParams) =
            match req.extract::<lsp_types::TypeHierarchySupertypesParams>(
                TypeHierarchySupertypes::METHOD,
            ) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid type-hierarchy-supertypes params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let result: Vec<lsp_types::TypeHierarchyItem> = self
            .file_id_for_uri(&params.item.uri)
            .map(|file_id| {
                let analysis = self.host.analysis();
                let thi = rua_analysis::TypeHierarchyItem {
                    name: params.item.name.clone(),
                    kind: rua_analysis::DefKind::Struct,
                    file_id,
                    range: rua_analysis::TextRange::new(0, 0),
                };
                analysis
                    .type_hierarchy_supertypes(&thi)
                    .into_iter()
                    .map(|item| {
                        let source =
                            analysis.parse(item.file_id).syntax_node().text().to_string();
                        let li = LineIndex::new(&source);
                        let (sl, sc) = li.line_col(item.range.start() as usize, &source);
                        let (el, ec) = li.line_col(item.range.end() as usize, &source);
                        lsp_types::TypeHierarchyItem { detail: None,
                            name: item.name,
                            kind: lsp_types::SymbolKind::INTERFACE,
                            uri: self
                                .uri_for_file(item.file_id)
                                .unwrap_or_else(|| {
                                    format!("file:///unknown/{}", item.file_id.index())
                                        .parse()
                                        .unwrap()
                                }),
                            range: Range {
                                start: Position::new(sl as u32, sc as u32),
                                end: Position::new(el as u32, ec as u32),
                            },
                            selection_range: Range {
                                start: Position::new(sl as u32, sc as u32),
                                end: Position::new(el as u32, ec as u32),
                            },
                            data: None,
                            tags: None,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- execute command (SSR) -----------------------------------------------

    #[allow(clippy::mutable_key_type)]
    fn handle_execute_command(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, lsp_types::ExecuteCommandParams) =
            match req.extract::<lsp_types::ExecuteCommandParams>(ExecuteCommand::METHOD) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid execute-command params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };

        if params.command == "rua.ssr" && params.arguments.len() >= 2 {
            let pattern = params.arguments[0].as_str().unwrap_or("");
            let replacement = params.arguments[1].as_str().unwrap_or("");
            let mut edits_map: std::collections::HashMap<Uri, Vec<TextEdit>> =
                std::collections::HashMap::new();

            // Search all open files for the pattern and replace.
            let file_ids: Vec<(FileId, Uri)> = self
                .file_ids
                .iter()
                .map(|(_, (uri, id))| (*id, uri.clone()))
                .collect();
            let analysis = self.host.analysis();
            // Empty pattern would cause find("") = Some(0) on every
            // iteration, wasting CPU. Guard early.
            if pattern.is_empty() || replacement.is_empty() {
                let resp = Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    "SSR: pattern and replacement must be non-empty".to_string(),
                );
                let _ = self.connection.sender.send(Message::Response(resp));
                return;
            }
            for (file_id, uri) in &file_ids {
                let source =
                    analysis.parse(*file_id).syntax_node().text().to_string();
                let line_index = LineIndex::new(&source);
                let mut file_edits = Vec::new();
                let mut search_start = 0usize;
                while let Some(pos) = source[search_start..].find(pattern) {
                    let abs_pos = search_start + pos;
                    if !pattern.is_empty() {
                        let (sl, sc) = line_index.line_col(abs_pos, &source);
                        let (el, ec) =
                            line_index.line_col(abs_pos + pattern.len(), &source);
                        file_edits.push(TextEdit {
                            range: Range {
                                start: Position::new(sl as u32, sc as u32),
                                end: Position::new(el as u32, ec as u32),
                            },
                            new_text: replacement.to_string(),
                        });
                    }
                    search_start = abs_pos + pattern.len().max(1);
                    if file_edits.len() > 100 {
                        break;
                    }
                }
                if !file_edits.is_empty() {
                    edits_map.insert(uri.clone(), file_edits);
                }
            }

            let result = if edits_map.is_empty() {
                None
            } else {
                Some(WorkspaceEdit {
                    changes: Some(edits_map),
                    ..Default::default()
                })
            };
            let resp = Response::new_ok(id, result);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        }

        let resp: Response =
            Response::new_ok(id, Option::<serde_json::Value>::None);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- workspace symbol ----------------------------------------------------

    fn handle_workspace_symbol(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, WorkspaceSymbolParams) =
            match req.extract::<WorkspaceSymbolParams>(WorkspaceSymbolRequest::METHOD) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid workspace-symbol params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let query = params.query.to_lowercase();
        let analysis = self.host.analysis();
        let mut symbols: Vec<WorkspaceSymbol> = Vec::new();

        // Search across all open files.
        let file_ids: Vec<FileId> = self.file_ids.values().map(|(_, id)| *id).collect();
        for file_id in file_ids {
            let def_map = analysis.def_map(file_id);
            for definition in def_map.definitions() {
                let name = definition.name();
                if name.to_lowercase().contains(&query) {
                    let kind = match definition.kind() {
                        rua_analysis::DefKind::Function | rua_analysis::DefKind::ExternFunction
                        | rua_analysis::DefKind::Method => LspSymbolKind::FUNCTION,
                        rua_analysis::DefKind::Struct => LspSymbolKind::STRUCT,
                        rua_analysis::DefKind::Enum => LspSymbolKind::ENUM,
                        rua_analysis::DefKind::Trait => LspSymbolKind::INTERFACE,
                        rua_analysis::DefKind::Module => LspSymbolKind::MODULE,
                        rua_analysis::DefKind::Variant => LspSymbolKind::ENUM_MEMBER,
                        rua_analysis::DefKind::Field => LspSymbolKind::FIELD,
                        rua_analysis::DefKind::TypeAlias => LspSymbolKind::TYPE_PARAMETER,
                        _ => LspSymbolKind::OBJECT,
                    };
                    if let Some(location) = self.nav_to_location(
                        &rua_analysis::NavigationTarget::new(
                            rua_analysis::FileRange::new(
                                definition.file_id(),
                                definition.name_range(),
                            ),
                            None,
                        ),
                    ) {
                        let (uri, range) = match location {
                            GotoDefinitionResponse::Scalar(l) => (l.uri, l.range),
                            _ => continue,
                        };
                        symbols.push(WorkspaceSymbol {
                            name: name.to_string(),
                            kind,
                            location: OneOf::Left(Location { uri, range }),
                            container_name: None,
                            tags: None,
                            data: None,
                        });
                    }
                }
                if symbols.len() >= 50 {
                    break;
                }
            }
            if symbols.len() >= 50 {
                break;
            }
        }

        let resp = Response::new_ok(id, WorkspaceSymbolResponse::Nested(symbols));
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- folding range -------------------------------------------------------

    fn handle_folding_range(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, FoldingRangeParams) =
            match req.extract::<FoldingRangeParams>(FoldingRangeRequest::METHOD) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid folding-range params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let Some(file_id) = self.file_id_for_uri(&params.text_document.uri) else {
            let resp = Response::new_ok(id, Option::<Vec<FoldingRange>>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };

        let analysis = self.host.analysis();
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let line_index = LineIndex::new(&source);
        let mut ranges = Vec::new();
        let mut brace_stack: Vec<(usize, usize)> = Vec::new(); // (line, col)

        // Simple brace-based folding.
        for (i, ch) in source.bytes().enumerate() {
            if ch == b'{' {
                let (l, c) = line_index.line_col(i, &source);
                brace_stack.push((l, c));
            } else if ch == b'}'
                && let Some((start_line, start_col)) = brace_stack.pop()
            {
                let (end_line, end_col) = line_index.line_col(i + 1, &source);
                if end_line > start_line {
                    ranges.push(FoldingRange {
                        start_line: start_line as u32,
                        start_character: Some(start_col as u32),
                        end_line: end_line as u32,
                        end_character: Some(end_col as u32),
                        kind: Some(FoldingRangeKind::Region),
                        collapsed_text: None,
                    });
                }
            }
        }

        // Doc comment folding: consecutive `///` lines.
        let lines: Vec<&str> = source.lines().collect();
        let mut doc_start: Option<usize> = None;
        for (i, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with("///") {
                if doc_start.is_none() {
                    doc_start = Some(i);
                }
            } else if let Some(start) = doc_start {
                if i - start > 1 {
                    ranges.push(FoldingRange {
                        start_line: start as u32,
                        start_character: None,
                        end_line: (i - 1) as u32,
                        end_character: Some(80),
                        kind: Some(FoldingRangeKind::Comment),
                        collapsed_text: None,
                    });
                }
                doc_start = None;
            }
        }
        if let Some(start) = doc_start
            && lines.len() - start > 1
        {
            ranges.push(FoldingRange {
                start_line: start as u32,
                start_character: None,
                end_line: (lines.len() - 1) as u32,
                end_character: Some(80),
                kind: Some(FoldingRangeKind::Comment),
                collapsed_text: None,
            });
        }

        let resp = Response::new_ok(id, ranges);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- document link -------------------------------------------------------

    fn handle_document_link(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, DocumentLinkParams) =
            match req.extract::<DocumentLinkParams>(DocumentLinkRequest::METHOD) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid document-link params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let Some(file_id) = self.file_id_for_uri(&params.text_document.uri) else {
            let resp = Response::new_ok(id, Option::<Vec<DocumentLink>>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };

        let analysis = self.host.analysis();
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let line_index = LineIndex::new(&source);
        let mut links = Vec::new();

        // Find `[text]` references in doc comments and link them.
        for (line_num, line) in source.lines().enumerate() {
            let trimmed = line.trim_start();
            if !trimmed.starts_with("///") {
                continue;
            }
            let line_start = line_index.offset(line_num, 0, &source);
            let mut search_start = line_start;
            while let Some(open) = source[search_start..].find('[') {
                let abs_open = search_start + open;
                if let Some(close) = source[abs_open..].find(']') {
                    let abs_close = abs_open + close + 1;
                    let link_text = &source[abs_open + 1..abs_close - 1];
                    if !link_text.is_empty() && !link_text.contains('\n') {
                        let (sl, sc) = line_index.line_col(abs_open, &source);
                        let (el, ec) = line_index.line_col(abs_close, &source);
                        let target = format!("file:///{link_text}");
                        if let Ok(target_uri) = target.parse::<Uri>() {
                            links.push(DocumentLink {
                                range: Range {
                                    start: Position::new(sl as u32, sc as u32),
                                    end: Position::new(el as u32, ec as u32),
                                },
                                target: Some(target_uri),
                                tooltip: None,
                                data: None,
                            });
                        }
                    }
                    search_start = abs_close;
                } else {
                    break;
                }
            }
        }

        let resp = Response::new_ok(id, links);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- completion resolve --------------------------------------------------

    fn handle_resolve_completion(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, mut item): (_, CompletionItem) =
            match req.extract::<CompletionItem>(ResolveCompletionItem::METHOD) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid resolve params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };

        // If item already has documentation, nothing to do.
        if item.documentation.is_some() {
            let resp = Response::new_ok(id, item);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        }

        // Deserialize the completion data and try to look up documentation.
        if let Some(raw) = &item.data
            && let Some((raw_file_id, name)) = parse_resolve_data(raw)
        {
                let file_id = rua_analysis::FileId::new(raw_file_id);
                let analysis = self.host.analysis();
                let def_map = analysis.def_map(file_id);
                // Find the definition by name and extract its doc comment.
                if let Some(definition) = def_map
                    .definitions()
                    .find(|d| d.name() == name && d.file_id() == file_id)
                {
                    // Walk backwards from the definition keyword to collect
                    // /// line comments.
                    let parse = analysis.parse(file_id);
                    let root = parse.syntax_node();
                    let range_start: u32 = definition.range().start();
                    let end: u32 = root.text_range().end().into();
                    // Find token at the start of the definition.
                    let mut current: Option<rua_syntax::SyntaxToken> = match root
                        .token_at_offset(range_start.min(end).into())
                    {
                        rowan::TokenAtOffset::Single(t) => Some(t),
                        rowan::TokenAtOffset::Between(l, _) => Some(l),
                        _ => None,
                    };
                    let mut doc_lines: Vec<String> = Vec::new();
                    loop {
                        let prev = current.as_ref().and_then(|t| t.prev_token());
                        match prev {
                            None => break,
                            Some(ref pt)
                                if pt.kind() == rua_syntax::SyntaxKind::LineComment
                                    && pt.text().starts_with("///") =>
                            {
                                let doc = pt
                                    .text()
                                    .strip_prefix("///")
                                    .unwrap_or(pt.text())
                                    .trim();
                                doc_lines.push(doc.to_string());
                                current = prev;
                            }
                            Some(ref pt) if pt.kind().is_trivia() => {
                                current = prev;
                            }
                            Some(_) => break,
                        }
                    }
                    if !doc_lines.is_empty() {
                        doc_lines.reverse();
                        item.documentation = Some(Documentation::MarkupContent(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: doc_lines.join("\n"),
                        }));
                    }
                }
        }

        let resp = Response::new_ok(id, item);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- references ----------------------------------------------------------

    fn handle_references(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params) =
            match req.extract::<lsp_types::ReferenceParams>(References::METHOD) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid references params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let include_decl = params.context.include_declaration;

        let locations = self
            .project_position(uri, pos)
            .map(|pp| {
                let analysis = self.host.analysis();
                let refs = analysis.references(pp, include_decl);
                refs.into_iter()
                    .filter_map(|r| self.ref_to_location(&r))
                    .collect::<Vec<_>>()
            });

        let resp = Response::new_ok(id, locations);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- rename / prepare rename ---------------------------------------------

    #[allow(clippy::mutable_key_type)]
    fn handle_rename(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params) = match req.extract::<lsp_types::RenameParams>(Rename::METHOD) {
            Ok(v) => v,
            Err(e) => {
                let resp = Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    format!("invalid rename params: {e:?}"),
                );
                let _ = self.connection.sender.send(Message::Response(resp));
                return;
            }
        };
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;

        let result = self
            .project_position(uri, pos)
            .and_then(|pp| {
                let analysis = self.host.analysis();
                match analysis.rename(pp, &params.new_name) {
                    Ok(change) => source_change_to_workspace_edit(
                        &analysis,
                        &change,
                        |fid| self.file_to_uri.get(&fid).cloned(),
                    ),
                    Err(_) => None,
                }
            });

        match result {
            Some(edit) => {
                let resp = Response::new_ok(id, edit);
                let _ = self.connection.sender.send(Message::Response(resp));
            }
            None => {
                let resp = Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    "cannot rename at this position".to_string(),
                );
                let _ = self.connection.sender.send(Message::Response(resp));
            }
        }
    }

    fn handle_prepare_rename(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params) =
            match req.extract::<lsp_types::TextDocumentPositionParams>(
                PrepareRenameRequest::METHOD,
            ) {
                Ok(v) => v,
                Err(e) => {
                    let resp = Response::new_err(
                        id,
                        lsp_server::ErrorCode::InvalidParams as i32,
                        format!("invalid prepare-rename params: {e:?}"),
                    );
                    let _ = self.connection.sender.send(Message::Response(resp));
                    return;
                }
            };
        let uri = &params.text_document.uri;
        let pos = params.position;

        let result = self
            .project_position(uri, pos)
            .and_then(|pp| {
                let analysis = self.host.analysis();
                analysis.prepare_rename(pp)
            })
            .and_then(|target| {
                self.range_for_file(target.range().file_id, target.range().range)
                    .map(PrepareRenameResponse::Range)
            });

        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- semantic tokens -----------------------------------------------------

    fn handle_semantic_tokens(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params) = match req.extract::<lsp_types::SemanticTokensParams>(
            SemanticTokensFullRequest::METHOD,
        ) {
            Ok(value) => value,
            Err(error) => {
                let response = Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    format!("invalid semantic-token params: {error:?}"),
                );
                let _ = self.connection.sender.send(Message::Response(response));
                return;
            }
        };
        let uri = &params.text_document.uri;
        let Some(file_id) = self.file_id_for_uri(uri) else {
            let resp = Response::new_ok(
                id,
                Option::<SemanticTokensResult>::None,
            );
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };

        let analysis = self.host.analysis();
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let line_index = LineIndex::new(&source);
        let tokens = analysis.semantic_tokens(file_id);
        let data = encode_semantic_tokens(&tokens, &line_index, &source);

        let result = SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        });
        let resp = Response::new_ok(id, Some(result));
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    fn handle_semantic_tokens_range(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params) = match req
            .extract::<lsp_types::SemanticTokensRangeParams>(SemanticTokensRangeRequest::METHOD)
        {
            Ok(value) => value,
            Err(error) => {
                let response = Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    format!("invalid semantic-token-range params: {error:?}"),
                );
                let _ = self.connection.sender.send(Message::Response(response));
                return;
            }
        };
        let uri = &params.text_document.uri;
        let Some(file_id) = self.file_id_for_uri(uri) else {
            let resp = Response::new_ok(id, Option::<SemanticTokensResult>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
        let analysis = self.host.analysis();
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let line_index = LineIndex::new(&source);
        let tokens = analysis.semantic_tokens(file_id);
        // Filter tokens to those overlapping the requested range.
        let range_start = line_index.offset(
            params.range.start.line as usize,
            params.range.start.character as usize,
            &source,
        );
        let range_end = line_index.offset(
            params.range.end.line as usize,
            params.range.end.character as usize,
            &source,
        );
        let filtered: Vec<_> = tokens
            .into_iter()
            .filter(|t| {
                let s = t.range().start() as usize;
                let e = t.range().end() as usize;
                s < range_end && e > range_start
            })
            .collect();
        let data = encode_semantic_tokens(&filtered, &line_index, &source);
        let result = SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        });
        let resp = Response::new_ok(id, Some(result));
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- notifications -------------------------------------------------------

    fn handle_notification(&mut self, not: Notification) {
        match not.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let params: lsp_types::DidOpenTextDocumentParams =
                    match serde_json::from_value(not.params) {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("rua-lsp: bad didOpen params: {e}");
                            return;
                        }
                    };
                self.open_document(params.text_document.uri, params.text_document.text);
            }
            DidChangeTextDocument::METHOD => {
                let params: lsp_types::DidChangeTextDocumentParams =
                    match serde_json::from_value(not.params) {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("rua-lsp: bad didChange params: {e}");
                            return;
                        }
                    };
                if let Some(change) = params.content_changes.last() {
                    self.change_document(
                        params.text_document.uri,
                        change.text.clone(),
                    );
                }
            }
            DidCloseTextDocument::METHOD => {
                let params: lsp_types::DidCloseTextDocumentParams =
                    match serde_json::from_value(not.params) {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("rua-lsp: bad didClose params: {e}");
                            return;
                        }
                    };
                self.close_document(params.text_document.uri);
            }
            DidSaveTextDocument::METHOD => {
                let params: lsp_types::DidSaveTextDocumentParams =
                    match serde_json::from_value(not.params) {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("rua-lsp: bad didSave params: {e}");
                            return;
                        }
                    };
                self.handle_did_save(&params);
            }
            DidChangeConfiguration::METHOD => {
                let params: lsp_types::DidChangeConfigurationParams =
                    match serde_json::from_value(not.params) {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("rua-lsp: bad didChangeConfiguration params: {e}");
                            return;
                        }
                    };
                self.reload_configuration(&params.settings);
            }
            DidChangeWatchedFiles::METHOD => {
                let params: DidChangeWatchedFilesParams =
                    match serde_json::from_value(not.params) {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("rua-lsp: bad didChangeWatchedFiles params: {e}");
                            return;
                        }
                    };
                self.handle_watched_file_change(&params);
            }
            _ => {}
        }
    }

    fn open_document(&mut self, uri: Uri, text: String) {
        let file_id = self.ensure_file_id(&uri);
        let mut change = Change::new();
        change.set_file_text(file_id, &*text);
        self.host.apply_change(change);
        self.open_buffers.insert(file_id, (uri.clone(), text));
        self.publish_diagnostics(&uri);
    }

    fn change_document(&mut self, uri: Uri, text: String) {
        let file_id = self.ensure_file_id(&uri);
        let mut change = Change::new();
        change.set_file_text(file_id, &*text);
        self.host.apply_change(change);
        self.open_buffers.insert(file_id, (uri.clone(), text));
        self.publish_diagnostics(&uri);
    }

    fn close_document(&mut self, uri: Uri) {
        let key = Self::doc_key(&uri);
        if let Some((_, file_id)) = self.file_ids.get(&key).map(|(u, f)| (u.clone(), *f)) {
            self.open_buffers.remove(&file_id);
            // Remove from host so it doesn't keep stale open-buffer state
            let mut change = Change::new();
            change.remove_file(file_id);
            self.host.apply_change(change);
        }
        self.send_diagnostics(&uri, &[]);
    }

    fn handle_did_save(&mut self, params: &lsp_types::DidSaveTextDocumentParams) {
        let uri = &params.text_document.uri;
        let Some(file_id) = self.file_id_for_uri(uri) else { return };
        let analysis = self.host.analysis();
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let line_index = LineIndex::new(&source);

        // Resolve the actual file path from the URI so we can pass it to
        // `ruac check` (which expects a path, not source text).
        let file_path = uri_to_path(uri).unwrap_or_else(|| PathBuf::from(uri.as_str()));

        // Try to run ruac check as a subprocess.
        if let Ok(output) = std::process::Command::new("ruac")
            .arg("check")
            .arg(&file_path)
            .output()
            && !output.status.success()
        {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut diags: Vec<Diagnostic> = Vec::new();
            for line in stderr.lines() {
                if let Some(rest) = line.strip_prefix("error:") {
                    let msg = rest.trim().to_string();
                    let (sl, sc) = line_index.line_col(0, &source);
                    let range = Range {
                        start: Position::new(sl as u32, sc as u32),
                        end: Position::new(sl as u32, 80.min(source.len() as u32)),
                    };
                    diags.push(Diagnostic {
                        range,
                        severity: Some(DiagnosticSeverity::ERROR),
                        source: Some("ruac".to_string()),
                        message: msg,
                        ..Default::default()
                    });
                }
            }
            if !diags.is_empty() {
                self.send_diagnostics(uri, &diags);
                return;
            }
        }
        // Fall back to native diagnostics.
        self.publish_diagnostics(uri);
    }

    fn reload_configuration(&mut self, settings: &serde_json::Value) {
        // Extract library roots from settings
        let config = LibraryConfig::from_settings(settings);
        if let Ok(config) = config {
            let mut change = Change::new();
            let root_id = SourceRootId::new(self.next_root_id);
            self.next_root_id += 1;

            change.set_source_root(root_id, SourceRootKind::Library);
            for (path, content) in config.files {
                let file_id = self.ensure_file_id_for_path(&path);
                change.set_file(file_id, root_id, FileKind::Declaration, &*content);
            }
            self.host.apply_change(change);
            self.library_roots = config.roots;
            self.library_mounts = config.mounts;
            self.register_watchers();
        }
    }

    // -- file watchers --------------------------------------------------------

    /// Register `workspace/didChangeWatchedFiles` for configured library roots.
    fn register_watchers(&mut self) {
        let mut watchers: Vec<FileSystemWatcher> = Vec::new();

        for root in &self.library_roots {
            if let Ok(canonical) = std::fs::canonicalize(root) {
                let pattern = if canonical.is_dir() {
                    canonical.join("**/*.ruai")
                } else {
                    canonical
                };
                let glob = pattern.to_string_lossy().to_string();
                // Only register if not already watching this path.
                if !self.watched_paths.iter().any(|p| p.to_string_lossy() == glob) {
                    watchers.push(FileSystemWatcher {
                        glob_pattern: lsp_types::GlobPattern::String(glob.clone()),
                        kind: Some(WatchKind::all()),
                    });
                    self.watched_paths.push(PathBuf::from(&glob));
                }
            }
        }
        for mount_path in self.library_mounts.values() {
            if let Ok(canonical) = std::fs::canonicalize(mount_path) {
                let glob = canonical.to_string_lossy().to_string();
                if !self.watched_paths.iter().any(|p| p.to_string_lossy() == glob) {
                    watchers.push(FileSystemWatcher {
                        glob_pattern: lsp_types::GlobPattern::String(glob.clone()),
                        kind: Some(WatchKind::all()),
                    });
                    self.watched_paths.push(PathBuf::from(&glob));
                }
            }
        }

        if watchers.is_empty() {
            return;
        }

        let registration = Registration {
            id: "rua-library-watcher".to_string(),
            method: DidChangeWatchedFiles::METHOD.to_string(),
            register_options: Some(
                serde_json::to_value(lsp_types::DidChangeWatchedFilesRegistrationOptions {
                    watchers,
                })
                .unwrap_or_default(),
            ),
        };
        let params = RegistrationParams {
            registrations: vec![registration],
        };
        let request = lsp_server::Request::new(
            1.into(), // id for the registration request
            "client/registerCapability".to_string(),
            params,
        );
        let _ = self
            .connection
            .sender
            .send(lsp_server::Message::Request(request));
    }

    fn handle_watched_file_change(&mut self, params: &DidChangeWatchedFilesParams) {
        // Debounce: skip if within 100ms of the last event.
        let now = std::time::Instant::now();
        if let Some(last) = self.last_watcher_event
            && now.duration_since(last) < std::time::Duration::from_millis(100) {
                return;
            }
        self.last_watcher_event = Some(now);

        let mut change = Change::new();
        let root_id = SourceRootId::new(self.next_root_id);
        // Use the same root as library files (increment for reload).
        self.next_root_id += 1;
        change.set_source_root(root_id, SourceRootKind::Library);

        for event in &params.changes {
            let Some(path) = uri_to_path(&event.uri) else {
                continue;
            };
            let file_id = self.ensure_file_id_for_path(&path);
            match event.typ {
                lsp_types::FileChangeType::CREATED | lsp_types::FileChangeType::CHANGED => {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        change.set_file(file_id, root_id, FileKind::Declaration, &*text);
                    }
                }
                lsp_types::FileChangeType::DELETED => {
                    change.remove_file(file_id);
                }
                _ => {}
            }
        }

        self.host.apply_change(change);

        // Republish diagnostics for any open files that may be affected.
        let open_uris: Vec<Uri> =
            self.open_buffers.values().map(|(uri, _)| uri.clone()).collect();
        for uri in open_uris {
            self.publish_diagnostics(&uri);
        }
    }

    fn ensure_file_id_for_path(&mut self, path: &Path) -> FileId {
        if let Some((_, id)) = self.file_ids.get(path) {
            return *id;
        }
        let id = FileId::new(self.next_file_id);
        self.next_file_id += 1;
        let uri = path_to_uri(path).unwrap_or_else(|| {
            format!("file:///unknown/{}", self.next_file_id)
                .parse()
                .unwrap_or_else(|_| "file:///unknown.rua".parse().unwrap())
        });
        self.file_ids
            .insert(path.to_path_buf(), (uri.clone(), id));
        self.file_to_uri.insert(id, uri);
        id
    }

    // -- helpers -------------------------------------------------------------

    fn project_position(&self, uri: &Uri, pos: Position) -> Option<ProjectPosition> {
        let file_id = self.file_id_for_uri(uri)?;
        let analysis = self.host.analysis();
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let line_index = LineIndex::new(&source);
        let offset = line_index.offset(pos.line as usize, pos.character as usize, &source);
        Some(ProjectPosition::at(
            ProjectId::new(0),
            file_id,
            offset as u32,
        ))
    }

    fn range_for_file(&self, file_id: FileId, range: TextRange) -> Option<Range> {
        let analysis = self.host.analysis();
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let li = LineIndex::new(&source);
        let start = li.line_col(range.start() as usize, &source);
        let end = li.line_col(range.end() as usize, &source);
        Some(Range {
            start: Position::new(start.0 as u32, start.1 as u32),
            end: Position::new(end.0 as u32, end.1 as u32),
        })
    }

    fn nav_to_location(&self, target: &NavigationTarget) -> Option<GotoDefinitionResponse> {
        let file_range = target.target_range();
        let uri = self.uri_for_file(file_range.file_id)?;
        let range = self.range_for_file(file_range.file_id, file_range.range)?;
        Some(GotoDefinitionResponse::Scalar(Location { uri, range }))
    }

    fn ref_to_location(
        &self,
        r: &rua_analysis::ReferenceResult,
    ) -> Option<Location> {
        let file_range = r.range();
        let uri = self.uri_for_file(file_range.file_id)?;
        let range = self.range_for_file(file_range.file_id, file_range.range)?;
        Some(Location { uri, range })
    }

    fn publish_diagnostics(&mut self, uri: &Uri) {
        let Some(file_id) = self.file_id_for_uri(uri) else {
            return;
        };
        let analysis = self.host.analysis();
        let source = analysis.parse(file_id).syntax_node().text().to_string();
        let line_index = LineIndex::new(&source);
        let native = analysis.diagnostics(file_id);

        let lsp_diags: Vec<Diagnostic> = native
            .iter()
            .map(|d| core_diag_to_lsp(d, &line_index, &source))
            .collect();

        self.send_diagnostics(uri, &lsp_diags);
    }

    fn send_diagnostics(&self, uri: &Uri, diags: &[Diagnostic]) {
        let params = PublishDiagnosticsParams {
            uri: uri.clone(),
            diagnostics: diags.to_vec(),
            version: None,
        };
        let not = Notification::new(PublishDiagnostics::METHOD.to_string(), params);
        let _ = self.connection.sender.send(Message::Notification(not));
    }
}

// ---------------------------------------------------------------------------
// Library configuration (minimal — mirrors old analysis_inputs logic)
// ---------------------------------------------------------------------------

struct LibraryConfig {
    roots: Vec<PathBuf>,
    mounts: HashMap<String, PathBuf>,
    files: Vec<(PathBuf, String)>,
}

impl LibraryConfig {
    fn from_settings(settings: &serde_json::Value) -> Result<Self, String> {
        let nested = settings.get("rua");
        let library = settings
            .get("rua.library")
            .or_else(|| nested.and_then(|rua| rua.get("library")))
            .or_else(|| settings.get("library"));
        let mounts = settings
            .get("rua.libraryMounts")
            .or_else(|| nested.and_then(|rua| rua.get("libraryMounts")))
            .or_else(|| settings.get("libraryMounts"));

        let roots = parse_library_paths(library)?;
        let mounts = parse_mounts(mounts)?;

        let mut files = Vec::new();
        for root in &roots {
            scan_library_root(root, &mut files);
        }
        for path in mounts.values() {
            if let Ok(canonical) = std::fs::canonicalize(path)
                && let Ok(text) = std::fs::read_to_string(&canonical) {
                    files.push((canonical, text));
                }
        }

        Ok(LibraryConfig {
            roots,
            mounts,
            files,
        })
    }
}

fn parse_library_paths(value: Option<&serde_json::Value>) -> Result<Vec<PathBuf>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let entries = value
        .as_array()
        .ok_or_else(|| "rua.library must be an array of paths".to_string())?;
    entries
        .iter()
        .map(|v| {
            v.as_str()
                .map(PathBuf::from)
                .ok_or_else(|| "rua.library entry must be a string path".to_string())
        })
        .collect()
}

fn parse_mounts(
    value: Option<&serde_json::Value>,
) -> Result<HashMap<String, PathBuf>, String> {
    let Some(value) = value else {
        return Ok(HashMap::new());
    };
    let entries = value
        .as_object()
        .ok_or_else(|| "rua.libraryMounts must be an object".to_string())?;
    entries
        .iter()
        .map(|(k, v)| {
            v.as_str()
                .map(|p| (k.clone(), PathBuf::from(p)))
                .ok_or_else(|| format!("rua.libraryMounts.{k} must be a string path"))
        })
        .collect()
}

fn scan_library_root(root: &Path, files: &mut Vec<(PathBuf, String)>) {
    if let Ok(canonical) = std::fs::canonicalize(root) {
        if canonical.is_file() {
            if let Ok(text) = std::fs::read_to_string(&canonical) {
                files.push((canonical, text));
            }
        } else if canonical.is_dir() {
            scan_dir(&canonical, files);
        }
    }
}

fn scan_dir(dir: &Path, files: &mut Vec<(PathBuf, String)>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                scan_dir(&path, files);
            } else if path.extension().and_then(|e| e.to_str()) == Some("ruai")
                && let Ok(text) = std::fs::read_to_string(&path) {
                    files.push((path, text));
                }
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    eprintln!("rua-lsp: starting...");

    let (connection, io_threads) = Connection::stdio();

    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        document_on_type_formatting_provider: Some(
            lsp_types::DocumentOnTypeFormattingOptions {
                first_trigger_character: "\n".to_string(),
                more_trigger_character: None,
            },
        ),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        selection_range_provider: Some(
            lsp_types::SelectionRangeProviderCapability::Simple(true),
        ),
        call_hierarchy_provider: Some(lsp_types::CallHierarchyServerCapability::Simple(true)),
        code_lens_provider: Some(lsp_types::CodeLensOptions {
            resolve_provider: Some(false),
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        implementation_provider: Some(
            lsp_types::ImplementationProviderCapability::Simple(true),
        ),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        folding_range_provider: Some(lsp_types::FoldingRangeProviderCapability::from(true)),
        document_link_provider: Some(lsp_types::DocumentLinkOptions {
            resolve_provider: Some(false),
            work_done_progress_options: lsp_types::WorkDoneProgressOptions::default(),
        }),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: lsp_types::WorkDoneProgressOptions::default(),
        })),
        document_highlight_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(lsp_types::CodeActionProviderCapability::from(true)),
        inlay_hint_provider: Some(OneOf::Left(true)),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".into(), ",".into()]),
            retrigger_characters: Some(vec![",".into()]),
            ..Default::default()
        }),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![":".into(), ".".into()]),
            resolve_provider: Some(true),
            ..Default::default()
        }),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: semantic_token_legend(),
                range: Some(true),
                full: Some(SemanticTokensFullOptions::Bool(true)),
                ..SemanticTokensOptions::default()
            },
        )),
        ..ServerCapabilities::default()
    };

    let server_capabilities =
        serde_json::to_value(&capabilities).expect("serialize ServerCapabilities");
    let init_params = connection
        .initialize(server_capabilities)
        .expect("initialize handshake");

    let (workspace_folders, initialization_options): (Vec<Uri>, _) =
        serde_json::from_value::<InitializeParams>(init_params)
            .map(|p| {
                let folders = p
                    .workspace_folders
                    .unwrap_or_default()
                    .into_iter()
                    .map(|f| f.uri)
                    .collect();
                (folders, p.initialization_options)
            })
            .unwrap_or_default();

    let mut server = Server::new(connection);
    server.index_workspace_folders(&workspace_folders);
    if let Some(settings) = initialization_options {
        server.reload_configuration(&settings);
    }
    if let Err(e) = server.main_loop() {
        eprintln!("rua-lsp: error in main loop: {e}");
    }

    io_threads.join().expect("io threads join");
    eprintln!("rua-lsp: shutdown complete");
}

impl Server {
    fn index_workspace_folders(&mut self, folders: &[Uri]) {
        let root_id = SourceRootId::new(0); // workspace root
        let mut change = Change::new();
        change.set_source_root(root_id, SourceRootKind::Workspace);

        for uri in folders {
            if let Some(root) = uri_to_path(uri) {
                let mut count = 0u32;
                scan_workspace_files(&root, &mut |path, text| {
                    let file_id = self.ensure_file_id_for_path(path);
                    change.set_file(
                        file_id,
                        root_id,
                        FileKind::Source,
                        text,
                    );
                    count += 1;
                });
                eprintln!(
                    "rua-lsp: indexed {count} source(s) under {}",
                    root.display()
                );
            }
        }
        self.host.apply_change(change);
    }
}

fn scan_workspace_files(
    root: &Path,
    cb: &mut dyn FnMut(&Path, &str),
) {
    if let Ok(canonical) = std::fs::canonicalize(root) {
        if canonical.is_dir() {
            scan_workspace_dir(&canonical, cb);
        } else if canonical.is_file()
            && let Ok(text) = std::fs::read_to_string(&canonical) {
                cb(&canonical, &text);
            }
    }
}

fn scan_workspace_dir(dir: &Path, cb: &mut dyn FnMut(&Path, &str)) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                if !is_hidden(&path) {
                    scan_workspace_dir(&path, cb);
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("rua")
                && let Ok(text) = std::fs::read_to_string(&path) {
                    cb(&path, &text);
                }
        }
    }
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

// ---------------------------------------------------------------------------
// LSP type conversion
// ---------------------------------------------------------------------------

fn to_lsp_hover(hover: &HoverResult) -> Hover {
    let mut value = String::new();

    // Documentation (if present) — rendered as raw markdown.
    if let Some(doc) = hover.documentation()
        && !doc.is_empty()
    {
        value.push_str(doc);
        value.push_str("\n\n---\n\n");
    }

    // Signature in a code block with syntax highlighting.
    value.push_str(&format!("```rua\n{}\n```", hover.signature()));

    // Footer with clickable command links (VS Code extension).
    value.push_str("\n\n---\n\n");
    value.push_str("[Go to Definition](command:editor.action.revealDefinition) · ");
    value.push_str("[Find References](command:editor.action.referenceSearch.trigger) · ");
    value.push_str("[Peek Definition](command:editor.action.peekDefinition)");

    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
}

/// Minimal resolve token: (file_id, name) so the resolve handler can
/// look up doc comments lazily.
fn make_resolve_data(file_id: FileId, name: &str) -> Option<serde_json::Value> {
    serde_json::json!({
        "file_id": file_id.index(),
        "name": name,
    })
    .into()
}

fn parse_resolve_data(value: &serde_json::Value) -> Option<(u32, String)> {
    let file_id = value.get("file_id")?.as_u64()? as u32;
    let name = value.get("name")?.as_str()?.to_string();
    Some((file_id, name))
}

fn completion_to_lsp(
    item: &rua_analysis::CompletionItem,
    line_index: &LineIndex,
    source: &str,
    file_id: rua_analysis::FileId,
) -> CompletionItem {
    let kind = match item.kind() {
        CompletionKind::Keyword => Some(CompletionItemKind::KEYWORD),
        CompletionKind::Variable | CompletionKind::Parameter => {
            Some(CompletionItemKind::VARIABLE)
        }
        CompletionKind::Function => Some(CompletionItemKind::FUNCTION),
        CompletionKind::Method => Some(CompletionItemKind::METHOD),
        CompletionKind::Field => Some(CompletionItemKind::FIELD),
        CompletionKind::Struct => Some(CompletionItemKind::STRUCT),
        CompletionKind::Enum => Some(CompletionItemKind::ENUM),
        CompletionKind::Variant => Some(CompletionItemKind::ENUM_MEMBER),
        CompletionKind::Trait => Some(CompletionItemKind::INTERFACE),
        CompletionKind::Impl => Some(CompletionItemKind::CLASS),
        CompletionKind::Module => Some(CompletionItemKind::MODULE),
        CompletionKind::BuiltinType => Some(CompletionItemKind::STRUCT),
        CompletionKind::TypeAlias => Some(CompletionItemKind::TYPE_PARAMETER),
        CompletionKind::Macro => Some(CompletionItemKind::FUNCTION),
    };

    let (insert_text, insert_text_format) = match item.insert() {
        Some(CompletionInsert::Call { callee, params }) => {
            let snippet = if params.is_empty() {
                format!("{callee}($0)")
            } else {
                let placeholders: Vec<String> = params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| format!("${{{}:{}}}", i + 1, p))
                    .collect();
                format!("{callee}({})$0", placeholders.join(", "))
            };
            (Some(snippet), Some(InsertTextFormat::SNIPPET))
        }
        Some(CompletionInsert::MacroCall {
            name,
            delimiter,
        }) => {
            let snippet = match delimiter {
                MacroDelimiter::Parentheses => format!("{name}!($0)"),
                MacroDelimiter::Brackets => format!("{name}![$0]"),
                MacroDelimiter::Braces => format!("{name}!{{$0}}"),
            };
            (Some(snippet), Some(InsertTextFormat::SNIPPET))
        }
        Some(CompletionInsert::Plain(text)) => (Some(text.clone()), Some(InsertTextFormat::PLAIN_TEXT)),
        Some(CompletionInsert::Snippet(text)) => (Some(text.clone()), Some(InsertTextFormat::SNIPPET)),
        None => (None, None),
    };

    // sortText: invert relevance so higher relevance sorts first.
    let sort_text = Some(format!(
        "{:02}_{}",
        99u16.saturating_sub(item.relevance().min(99)),
        item.label()
    ));

    // text_edit: use replacement_range to create a proper TextEdit that
    // replaces the partial prefix at the cursor.
    let text_edit = item.replacement_range().and_then(|range| {
        let start = range.start() as usize;
        let end = range.end() as usize;
        if start > source.len() || end > source.len() {
            return None;
        }
        let (sl, sc) = line_index.line_col(start, source);
        let (el, ec) = line_index.line_col(end, source);
        Some(lsp_types::CompletionTextEdit::Edit(TextEdit {
            range: Range {
                start: Position::new(sl as u32, sc as u32),
                end: Position::new(el as u32, ec as u32),
            },
            new_text: item.label().to_string(),
        }))
    });

    // label_details: structured display — the detail part shows the type
    // signature next to the label, and description shows the origin.
    let label_details = {
        let detail = item.detail().map(|d| d.to_string());
        let description = match item.kind() {
            CompletionKind::Keyword => Some("keyword".to_string()),
            CompletionKind::Variable => Some("local".to_string()),
            CompletionKind::Parameter => Some("parameter".to_string()),
            CompletionKind::Function => Some("fn".to_string()),
            CompletionKind::Method => Some("method".to_string()),
            CompletionKind::Field => Some("field".to_string()),
            CompletionKind::Struct => Some("struct".to_string()),
            CompletionKind::Enum => Some("enum".to_string()),
            CompletionKind::Variant => Some("variant".to_string()),
            CompletionKind::Trait => Some("trait".to_string()),
            CompletionKind::Impl => None,
            CompletionKind::Module => Some("module".to_string()),
            CompletionKind::BuiltinType => Some("built-in type".to_string()),
            CompletionKind::TypeAlias => Some("type alias".to_string()),
            CompletionKind::Macro => Some("macro".to_string()),
        };
        if detail.is_some() || description.is_some() {
            Some(lsp_types::CompletionItemLabelDetails {
                detail,
                description,
            })
        } else {
            None
        }
    };

    // deprecated tag
    let deprecated = item.is_deprecated().then_some(true);
    let tags = if item.is_deprecated() {
        Some(vec![lsp_types::CompletionItemTag::DEPRECATED])
    } else {
        None
    };

    // data for resolve provider — stored as a JSON token so the resolve
    // handler can lazily look up documentation.
    let data = make_resolve_data(file_id, item.label());

    CompletionItem {
        label: item.label().to_string(),
        kind,
        detail: item.detail().map(|d| d.to_string()),
        documentation: item.documentation().map(|d| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: d.to_string(),
            })
        }),
        insert_text,
        insert_text_format,
        filter_text: item.lookup().map(|l| l.to_string()),
        sort_text,
        text_edit,
        label_details,
        deprecated,
        tags,
        data,
        additional_text_edits: item.import_path().map(|path| {
            let insert_line = find_import_insertion_point(source);
            let (il, _) = line_index.line_col(insert_line, source);
            vec![TextEdit {
                range: Range {
                    start: Position::new(il as u32, 0),
                    end: Position::new(il as u32, 0),
                },
                new_text: format!("{path}\n"),
            }]
        }),
        ..Default::default()
    }
}

fn core_diag_to_lsp(
    diagnostic: &rua_analysis::Diagnostic,
    line_index: &LineIndex,
    source: &str,
) -> Diagnostic {
    let range = diagnostic.range();
    let (start_line, start_column) =
        line_index.line_col(range.start() as usize, source);
    let (end_line, end_column) =
        line_index.line_col(range.end() as usize, source);
    let code_str = diagnostic.code().map(|c| c.error_code().to_string());
    let severity = match diagnostic.code() {
        Some(c) => match c.severity() {
            rua_analysis::DiagnosticSeverity::Error => Some(DiagnosticSeverity::ERROR),
            rua_analysis::DiagnosticSeverity::Warning => Some(DiagnosticSeverity::WARNING),
            rua_analysis::DiagnosticSeverity::Information => Some(DiagnosticSeverity::INFORMATION),
            rua_analysis::DiagnosticSeverity::Hint => Some(DiagnosticSeverity::HINT),
        },
        None => Some(DiagnosticSeverity::ERROR),
    };
    Diagnostic {
        range: Range {
            start: Position::new(start_line as u32, start_column as u32),
            end: Position::new(end_line as u32, end_column as u32),
        },
        severity,
        code: code_str.map(lsp_types::NumberOrString::String),
        source: Some("rua-analysis".to_string()),
        message: diagnostic.message().to_string(),
        ..Diagnostic::default()
    }
}

#[allow(clippy::mutable_key_type)]
fn source_change_to_workspace_edit(
    analysis: &rua_analysis::Analysis,
    change: &SourceChange,
    uri_for: impl Fn(rua_analysis::FileId) -> Option<Uri>,
) -> Option<WorkspaceEdit> {
    let mut edits = HashMap::new();
    for file_edit in change.file_edits() {
        let source = analysis
            .parse(file_edit.file_id())
            .syntax_node()
            .text()
            .to_string();
        let li = LineIndex::new(&source);
        let text_edits: Vec<TextEdit> = file_edit
            .edits()
            .iter()
            .map(|edit| {
                let (sl, sc) = li.line_col(edit.range().start() as usize, &source);
                let (el, ec) = li.line_col(edit.range().end() as usize, &source);
                TextEdit {
                    range: Range {
                        start: Position::new(sl as u32, sc as u32),
                        end: Position::new(el as u32, ec as u32),
                    },
                    new_text: edit.new_text().to_string(),
                }
            })
            .collect();
        let Some(uri) = uri_for(file_edit.file_id()) else {
            continue;
        };
        edits.insert(uri, text_edits);
    }
    if edits.is_empty() {
        return None;
    }
    Some(WorkspaceEdit {
        changes: Some(edits),
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// Semantic tokens
// ---------------------------------------------------------------------------

fn semantic_token_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::NAMESPACE,
            SemanticTokenType::TYPE,
            SemanticTokenType::STRUCT,
            SemanticTokenType::ENUM,
            SemanticTokenType::INTERFACE,
            SemanticTokenType::FUNCTION,
            SemanticTokenType::METHOD,
            SemanticTokenType::PROPERTY,
            SemanticTokenType::ENUM_MEMBER,
            SemanticTokenType::VARIABLE,
            SemanticTokenType::PARAMETER,
            SemanticTokenType::MACRO,
            SemanticTokenType::KEYWORD,
            SemanticTokenType::STRING,
            SemanticTokenType::NUMBER,
            SemanticTokenType::COMMENT,
            SemanticTokenType::OPERATOR,
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,
            SemanticTokenModifier::READONLY,
            SemanticTokenModifier::STATIC,
            SemanticTokenModifier::DEFAULT_LIBRARY,
            SemanticTokenModifier::new("unused"),
            SemanticTokenModifier::new("mutable"),
        ],
    }
}

const fn semantic_token_type_index(kind: SemanticTokenKind) -> u32 {
    match kind {
        SemanticTokenKind::Namespace => 0,
        SemanticTokenKind::Type => 1,
        SemanticTokenKind::Struct => 2,
        SemanticTokenKind::Enum => 3,
        SemanticTokenKind::Trait => 4,
        SemanticTokenKind::Function => 5,
        SemanticTokenKind::Method => 6,
        SemanticTokenKind::Property => 7,
        SemanticTokenKind::EnumMember => 8,
        SemanticTokenKind::Variable => 9,
        SemanticTokenKind::Parameter => 10,
        SemanticTokenKind::Macro => 11,
        SemanticTokenKind::Keyword => 12,
        SemanticTokenKind::String => 13,
        SemanticTokenKind::Number => 14,
        SemanticTokenKind::Comment => 15,
        SemanticTokenKind::Operator => 16,
    }
}

fn encode_semantic_tokens(
    tokens: &[rua_analysis::SemanticToken],
    line_index: &LineIndex,
    source: &str,
) -> Vec<LspSemanticToken> {
    let mut previous_line = 0u32;
    let mut previous_start = 0u32;
    let mut data = Vec::new();

    for token in tokens {
        let range = token.range();
        let start = range.start() as usize;
        let end = range.end() as usize;
        let (line, column) = line_index.line_col(start, source);
        let line = line as u32;
        let column = column as u32;
        let delta_line = line - previous_line;
        let delta_start = if delta_line == 0 {
            column - previous_start
        } else {
            column
        };
        let token_type = semantic_token_type_index(token.kind());
        data.push(LspSemanticToken {
            delta_line,
            delta_start,
            length: source[start..end].encode_utf16().count() as u32,
            token_type,
            token_modifiers_bitset: token.modifiers().bits(),
        });
        previous_line = line;
        previous_start = column;
    }

    data
}

// ---------------------------------------------------------------------------
// Document symbols
// ---------------------------------------------------------------------------

#[allow(deprecated)]
fn build_document_symbol_tree(
    symbols: &[rua_analysis::DocumentSymbol],
    li: &LineIndex,
    src: &str,
) -> Vec<DocumentSymbol> {
    symbols
        .iter()
        .map(|sym| {
            let children = build_document_symbol_tree(sym.children(), li, src);
            DocumentSymbol {
                name: sym.name().to_string(),
                detail: None,
                kind: to_lsp_symbol_kind(sym.kind()),
                range: range_from_bytes(sym.range(), li, src),
                selection_range: range_from_bytes(sym.selection_range(), li, src),
                children: if children.is_empty() {
                    None
                } else {
                    Some(children)
                },
                tags: None,
                deprecated: None,
            }
        })
        .collect()
}

fn to_lsp_symbol_kind(kind: DefKind) -> LspSymbolKind {
    match kind {
        DefKind::Function | DefKind::ExternFunction => LspSymbolKind::FUNCTION,
        DefKind::Struct => LspSymbolKind::STRUCT,
        DefKind::Enum => LspSymbolKind::ENUM,
        DefKind::Trait => LspSymbolKind::INTERFACE,
        DefKind::Impl => LspSymbolKind::OBJECT,
        DefKind::Method => LspSymbolKind::METHOD,
        DefKind::Field => LspSymbolKind::FIELD,
        DefKind::Variant => LspSymbolKind::ENUM_MEMBER,
        DefKind::Module => LspSymbolKind::MODULE,
        DefKind::TypeAlias => LspSymbolKind::TYPE_PARAMETER,
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn format_edits(src: &str) -> Vec<TextEdit> {
    let formatted = rua_syntax::format::format_str(src);
    if formatted == src {
        return Vec::new();
    }
    Vec::from([TextEdit {
        range: whole_document_range(src),
        new_text: formatted,
    }])
}

fn whole_document_range(src: &str) -> Range {
    let li = LineIndex::new(src);
    let (end_line, end_col) = li.line_col(src.len(), src);
    Range {
        start: Position::new(0, 0),
        end: Position::new(end_line as u32, end_col as u32),
    }
}

fn ranges_overlap_lsp(a: &Range, b: &Range) -> bool {
    !(a.end < b.start || b.end < a.start)
}

// ---------------------------------------------------------------------------
// URI / Path conversion
// ---------------------------------------------------------------------------

fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let s = uri.as_str();
    let path_str = s.strip_prefix("file://")?;
    let path_str = if let Some(rest) = path_str.strip_prefix("//") {
        rest
    } else {
        path_str
    };
    let decoded = percent_decode(path_str);
    Some(PathBuf::from(decoded))
}

/// Safe fallback URI when a real file path isn't available.
/// Uses a synthetic `file:///unknown/N` URI that won't crash.
fn fallback_uri(file_id: rua_analysis::FileId) -> Uri {
    format!("file:///unknown/{}", file_id.index())
        .parse()
        .unwrap_or_else(|_| "file:///unknown.rua".parse().unwrap())
}

/// Find the byte offset at which a new `use` import statement should be
/// inserted. Looks for the last existing import/module declaration; falls
/// back to after any initial comments, or position 0.
fn find_import_insertion_point(source: &str) -> usize {
    let mut last_import_end = 0usize;
    let mut pos = 0usize;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("use ") || trimmed.starts_with("mod ") {
            last_import_end = pos + line.len() + 1; // +1 for \n
        }
        pos += line.len() + 1;
    }
    if last_import_end > 0 {
        return last_import_end.min(source.len());
    }
    // No imports found — insert after any initial comment block.
    pos = 0;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            last_import_end = pos + line.len() + 1;
            pos += line.len() + 1;
        } else {
            break;
        }
    }
    last_import_end.min(source.len())
}

fn path_to_uri(path: &Path) -> Option<Uri> {
    let s = path.to_string_lossy();
    let encoded = percent_encode(&s);
    let uri_str = format!("file://{}", encoded);
    uri_str.parse().ok()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            ' ' => result.push_str("%20"),
            _ => result.push(c),
        }
    }
    result
}

fn range_from_bytes(
    range: rua_analysis::TextRange,
    li: &LineIndex,
    src: &str,
) -> Range {
    let start = (range.start() as usize).min(src.len());
    let end = (range.end() as usize).min(src.len());
    let (sl, sc) = li.line_col(start, src);
    let (el, ec) = li.line_col(end, src);
    Range {
        start: Position::new(sl as u32, sc as u32),
        end: Position::new(el as u32, ec as u32),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_ascii_space() {
        assert_eq!(percent_decode("a%20b"), "a b");
    }

    #[test]
    fn percent_decode_multibyte_utf8() {
        assert_eq!(percent_decode("%E4%B8%AD"), "中");
    }

    #[test]
    fn format_edits_noop_for_already_formatted() {
        let src = "fn main() {}\n";
        let formatted = rua_syntax::format::format_str(src);
        assert_eq!(formatted, src, "test fixture must be already formatted");
        assert!(format_edits(src).is_empty());
    }

    #[test]
    fn format_edits_noop_for_parse_error() {
        let src = "fn {";
        let edits = format_edits(src);
        assert!(edits.is_empty(), "parse-error input must produce no edits");
    }

    #[test]
    fn format_edits_produces_whole_document_edit() {
        let src = "fn main()   {}\n";
        let edits = format_edits(src);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].range.start, Position::new(0, 0));
        let expected_end = whole_document_range(src).end;
        assert_eq!(edits[0].range.end, expected_end);
        assert_eq!(edits[0].new_text, rua_syntax::format::format_str(src));
    }

    #[test]
    fn whole_document_range_multi_line() {
        let r = whole_document_range("ab\ncde\nf");
        assert_eq!(r.start, Position::new(0, 0));
        assert_eq!(r.end, Position::new(2, 1));
    }

    #[test]
    fn semantic_token_contract_legend_matches_analysis_kinds() {
        let legend = semantic_token_legend();
        let expected = [
            (SemanticTokenKind::Namespace, SemanticTokenType::NAMESPACE),
            (SemanticTokenKind::Type, SemanticTokenType::TYPE),
            (SemanticTokenKind::Struct, SemanticTokenType::STRUCT),
            (SemanticTokenKind::Enum, SemanticTokenType::ENUM),
            (SemanticTokenKind::Trait, SemanticTokenType::INTERFACE),
            (SemanticTokenKind::Function, SemanticTokenType::FUNCTION),
            (SemanticTokenKind::Method, SemanticTokenType::METHOD),
            (SemanticTokenKind::Property, SemanticTokenType::PROPERTY),
            (SemanticTokenKind::EnumMember, SemanticTokenType::ENUM_MEMBER),
            (SemanticTokenKind::Variable, SemanticTokenType::VARIABLE),
            (SemanticTokenKind::Parameter, SemanticTokenType::PARAMETER),
            (SemanticTokenKind::Macro, SemanticTokenType::MACRO),
            (SemanticTokenKind::Keyword, SemanticTokenType::KEYWORD),
            (SemanticTokenKind::String, SemanticTokenType::STRING),
            (SemanticTokenKind::Number, SemanticTokenType::NUMBER),
            (SemanticTokenKind::Comment, SemanticTokenType::COMMENT),
            (SemanticTokenKind::Operator, SemanticTokenType::OPERATOR),
        ];
        for (kind, token_type) in expected {
            assert_eq!(
                legend.token_types[semantic_token_type_index(kind) as usize],
                token_type
            );
        }
    }

    #[test]
    fn issue_tests_diagnostics() {
        let uri: Uri = "file:///test.rua".parse().unwrap();
        let (server_connection, _client_connection) = lsp_server::Connection::memory();
        let mut server = Server::new(server_connection);
        server.open_document(uri.clone(), "fn main() {}".to_string());
        // Should not panic
        server.publish_diagnostics(&uri);
    }

    #[test]
    fn server_starts_with_empty_state() {
        let (connection, _client) = lsp_server::Connection::memory();
        let server = Server::new(connection);
        assert!(server.file_ids.is_empty());
        assert!(server.open_buffers.is_empty());
    }

    // -- completion_to_lsp unit tests ---------------------------------------

    fn empty_line_index() -> LineIndex {
        LineIndex::new("")
    }

    #[test]
    fn sort_text_higher_relevance_sorts_first() {
        // Locals (relevance 95) should sort before keywords (relevance 50).
        let local = rua_analysis::CompletionItem::new("my_var", rua_analysis::CompletionKind::Variable)
            .with_detail("my_var: i64")
            .with_relevance(95);
        let keyword = rua_analysis::CompletionItem::new("fn", rua_analysis::CompletionKind::Keyword)
            .with_detail("keyword fn")
            .with_relevance(50);

        let li = empty_line_index();
        let lsp_local = completion_to_lsp(&local, &li, "", FileId::new(0));
        let lsp_kw = completion_to_lsp(&keyword, &li, "", FileId::new(0));

        let st_local = lsp_local.sort_text.as_deref().unwrap_or("");
        let st_kw = lsp_kw.sort_text.as_deref().unwrap_or("");
        assert!(
            st_local < st_kw,
            "local sort_text {st_local:?} must sort before keyword sort_text {st_kw:?}"
        );
    }

    #[test]
    fn sort_text_falls_back_to_label_for_equal_relevance() {
        let a = rua_analysis::CompletionItem::new("alpha", rua_analysis::CompletionKind::Variable)
            .with_relevance(80);
        let b = rua_analysis::CompletionItem::new("beta", rua_analysis::CompletionKind::Variable)
            .with_relevance(80);

        let li = empty_line_index();
        let lsp_a = completion_to_lsp(&a, &li, "", FileId::new(0));
        let lsp_b = completion_to_lsp(&b, &li, "", FileId::new(0));

        let st_a = lsp_a.sort_text.as_deref().unwrap_or("");
        let st_b = lsp_b.sort_text.as_deref().unwrap_or("");
        assert!(
            st_a < st_b,
            "alpha must sort before beta for equal relevance, got {st_a:?} >= {st_b:?}"
        );
    }

    #[test]
    fn macro_completion_has_snippet_insert_text() {
        let m = rua_analysis::CompletionItem::new("println!", rua_analysis::CompletionKind::Macro)
            .with_insert(rua_analysis::CompletionInsert::MacroCall {
                name: "println".to_string(),
                delimiter: rua_analysis::MacroDelimiter::Parentheses,
            });

        let li = empty_line_index();
        let lsp = completion_to_lsp(&m, &li, "", FileId::new(0));
        assert_eq!(lsp.insert_text.as_deref(), Some("println!($0)"));
        assert_eq!(lsp.insert_text_format, Some(lsp_types::InsertTextFormat::SNIPPET));
    }

    #[test]
    fn function_completion_has_call_snippet() {
        let f = rua_analysis::CompletionItem::new("greet", rua_analysis::CompletionKind::Function)
            .with_insert(rua_analysis::CompletionInsert::Call {
                callee: "greet".to_string(),
                params: vec![],
            });

        let li = empty_line_index();
        let lsp = completion_to_lsp(&f, &li, "", FileId::new(0));
        assert_eq!(lsp.insert_text.as_deref(), Some("greet($0)"));
        assert_eq!(lsp.insert_text_format, Some(lsp_types::InsertTextFormat::SNIPPET));
    }

    #[test]
    fn function_completion_with_params_has_placeholder_snippets() {
        let f = rua_analysis::CompletionItem::new("translate", rua_analysis::CompletionKind::Method)
            .with_insert(rua_analysis::CompletionInsert::Call {
                callee: "translate".to_string(),
                params: vec!["dx: i64".to_string(), "dy: i64".to_string()],
            });

        let li = empty_line_index();
        let lsp = completion_to_lsp(&f, &li, "", FileId::new(0));
        assert_eq!(
            lsp.insert_text.as_deref(),
            Some("translate(${1:dx: i64}, ${2:dy: i64})$0")
        );
        assert_eq!(lsp.insert_text_format, Some(lsp_types::InsertTextFormat::SNIPPET));
    }

    #[test]
    fn macro_completion_filter_text_excludes_bang() {
        let m = rua_analysis::CompletionItem::new("println!", rua_analysis::CompletionKind::Macro)
            .with_lookup("println")
            .with_insert(rua_analysis::CompletionInsert::MacroCall {
                name: "println".to_string(),
                delimiter: rua_analysis::MacroDelimiter::Parentheses,
            });

        let li = empty_line_index();
        let lsp = completion_to_lsp(&m, &li, "", FileId::new(0));
        assert_eq!(lsp.filter_text.as_deref(), Some("println"));
        // label still has the bang
        assert_eq!(lsp.label, "println!");
    }

    #[test]
    fn text_edit_converts_replacement_range() {
        let item = rua_analysis::CompletionItem::new("my_var", rua_analysis::CompletionKind::Variable)
            .with_replacement_range(rua_analysis::TextRange::new(5, 7));

        let source = "abc def ghi";
        // replacement range 5..7 = "de"
        let li = LineIndex::new(source);
        let lsp = completion_to_lsp(&item, &li, source, FileId::new(0));
        assert!(lsp.text_edit.is_some(), "text_edit should be set when replacement_range is set");
    }

    #[test]
    fn deprecated_item_has_deprecated_tag() {
        let item = rua_analysis::CompletionItem::new("old_fn", rua_analysis::CompletionKind::Function)
            .deprecated(true);

        let li = empty_line_index();
        let lsp = completion_to_lsp(&item, &li, "", FileId::new(0));
        assert_eq!(lsp.deprecated, Some(true));
        assert!(lsp.tags.is_some_and(|t| t.contains(&lsp_types::CompletionItemTag::DEPRECATED)));
    }

    #[test]
    fn normal_item_has_no_deprecated_tag() {
        let item = rua_analysis::CompletionItem::new("new_fn", rua_analysis::CompletionKind::Function);

        let li = empty_line_index();
        let lsp = completion_to_lsp(&item, &li, "", FileId::new(0));
        assert_eq!(lsp.deprecated, None);
        assert!(lsp.tags.is_none());
    }

    #[test]
    fn label_details_set_for_item() {
        let item = rua_analysis::CompletionItem::new("greet", rua_analysis::CompletionKind::Function)
            .with_detail("fn greet(name: String) -> String");

        let li = empty_line_index();
        let lsp = completion_to_lsp(&item, &li, "", FileId::new(0));
        let ld = lsp.label_details.as_ref().expect("label_details should be set");
        assert_eq!(ld.detail, Some("fn greet(name: String) -> String".to_string()));
        assert_eq!(ld.description, Some("fn".to_string()));
    }
}
