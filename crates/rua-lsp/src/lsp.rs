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
    DidOpenTextDocument, Notification as _, PublishDiagnostics,
};
use lsp_types::request::{
    Completion, DocumentSymbolRequest, Formatting, GotoDefinition, HoverRequest,
    PrepareRenameRequest, References, Rename, Request as _, SemanticTokensFullRequest,
};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionResponse, Diagnostic,
    DiagnosticSeverity, DidChangeWatchedFilesParams, DocumentFormattingParams, DocumentSymbol,
    DocumentSymbolResponse, Documentation, FileSystemWatcher, GotoDefinitionResponse, Hover,
    HoverContents, HoverProviderCapability, InitializeParams, InsertTextFormat, Location,
    MarkupContent, MarkupKind, OneOf, Position, PrepareRenameResponse, PublishDiagnosticsParams,
    Range, Registration, RegistrationParams, RenameOptions, ServerCapabilities,
    SemanticToken as LspSemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensResult,
    SemanticTokensServerCapabilities, SymbolKind as LspSymbolKind, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextEdit, Uri, WatchKind, WorkspaceEdit,
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
        self.open_buffers
            .get(&file_id)
            .map(|(uri, _)| uri.clone())
            .or_else(|| {
                self.file_ids
                    .iter()
                    .find(|(_, (_, id))| *id == file_id)
                    .map(|(_, (uri, _))| uri.clone())
            })
    }

    fn ensure_file_id(&mut self, uri: &Uri) -> FileId {
        let key = Self::doc_key(uri);
        if let Some((_, id)) = self.file_ids.get(&key) {
            return *id;
        }
        let id = FileId::new(self.next_file_id);
        self.next_file_id += 1;
        self.file_ids.insert(key, (uri.clone(), id));
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
            HoverRequest::METHOD => self.handle_hover(req),
            GotoDefinition::METHOD => self.handle_definition(req),
            DocumentSymbolRequest::METHOD => self.handle_document_symbol(req),
            Completion::METHOD => self.handle_completion(req),
            References::METHOD => self.handle_references(req),
            Rename::METHOD => self.handle_rename(req),
            PrepareRenameRequest::METHOD => self.handle_prepare_rename(req),
            SemanticTokensFullRequest::METHOD => self.handle_semantic_tokens(req),
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

        let items = self
            .project_position(uri, pos)
            .map(|pp| {
                let analysis = self.host.analysis();
                let native_items = analysis.completions(pp);
                native_items
                    .into_iter()
                    .map(|item| completion_to_lsp(&item))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let result = CompletionResponse::Array(items);
        let resp = Response::new_ok(id, result);
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
                    Ok(change) => source_change_to_workspace_edit(&analysis, &change),
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
            format!("file://{}", path.display()).parse().unwrap()
        });
        self.file_ids
            .insert(path.to_path_buf(), (uri, id));
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
        document_formatting_provider: Some(OneOf::Left(true)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: lsp_types::WorkDoneProgressOptions::default(),
        })),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![":".into(), ".".into()]),
            ..Default::default()
        }),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: semantic_token_legend(),
                range: None,
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
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::PlainText,
            value: hover.signature().to_string(),
        }),
        range: None,
    }
}

fn completion_to_lsp(item: &rua_analysis::CompletionItem) -> CompletionItem {
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
        Some(CompletionInsert::Call { callee, .. }) => {
            (Some(format!("{callee}($0)")), Some(InsertTextFormat::SNIPPET))
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
        None => (None, None),
    };

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
    Diagnostic {
        range: Range {
            start: Position::new(start_line as u32, start_column as u32),
            end: Position::new(end_line as u32, end_column as u32),
        },
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("rua-analysis".to_string()),
        message: diagnostic.message().to_string(),
        ..Diagnostic::default()
    }
}

#[allow(clippy::mutable_key_type)]
fn source_change_to_workspace_edit(
    analysis: &rua_analysis::Analysis,
    change: &SourceChange,
) -> Option<WorkspaceEdit> {
    let mut edits = HashMap::new();
    for file_edit in change.file_edits() {
        let analysis_snap = analysis; // use the same snapshot
        let source = analysis_snap
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
        // We need URI for file_edit — use a simple file:// URI
        let uri = format!("file:///unknown/{}", file_edit.file_id().index());
        if let Ok(uri) = uri.parse::<Uri>() {
            edits.insert(uri, text_edits);
        }
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
}
