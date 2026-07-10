//! `rua-lsp` — the Rua Language Server.
//!
//! Communicates via stdio JSON-RPC (LSP). Current scope: C1 live diagnostics +
//! C2 formatting + C3 hover / go-to-definition + C4 document symbols / completion.
//!
//! Architecture follows emmylua-analyzer-rust patterns (§8.1): single-threaded
//! synchronous loop, one handler module per feature.

use std::path::{Path, PathBuf};

mod analysis_inputs;

use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeConfiguration, DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument,
    Notification as _, PublishDiagnostics,
};
use lsp_types::request::{Completion, DocumentSymbolRequest, Formatting, GotoDefinition, HoverRequest, PrepareRenameRequest, References, Rename, Request as _, SemanticTokensFullRequest};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionResponse, Diagnostic,
    DiagnosticSeverity, DocumentFormattingParams, DocumentSymbol, DocumentSymbolResponse,
    Documentation, GotoDefinitionResponse, Hover, HoverContents, HoverProviderCapability,
    InitializeParams, InsertTextFormat, Location, MarkupContent, MarkupKind, OneOf, Position,
    PrepareRenameResponse, PublishDiagnosticsParams, Range, RenameOptions, ServerCapabilities,
    SemanticToken as LspSemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensResult,
    SemanticTokensServerCapabilities, SymbolKind as LspSymbolKind, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextEdit, Uri, WorkspaceEdit,
};

use rua_analysis::{
    AnalysisHost as CoreAnalysisHost, Change as CoreChange, Diagnostic as CoreDiagnostic,
    DiagnosticOrigin, FileId as CoreFileId, SemanticTokenKind, TextRange as CoreTextRange,
    reconcile_diagnostics,
};
use rua_syntax::analysis::{CompletionMember, LocalCompletion, MemberKind};
use rua_syntax::symbols::{Symbol, SymbolKind};
use rua_syntax::workspace::{DiskLoader, Workspace, normalize_path};
use rua_syntax::LineIndex;

use analysis_inputs::AnalysisInputs;

fn main() {
    eprintln!("rua-lsp: starting...");

    // Stdio transport (stderr is the LSP log channel).
    let (connection, io_threads) = Connection::stdio();

    // Advertise capabilities: full-text sync only. Diagnostics are delivered via
    // the *push* model (`textDocument/publishDiagnostics`), which needs no
    // capability. We intentionally do NOT advertise `diagnostic_provider` (the
    // LSP 3.17 *pull* model) since we don't answer `textDocument/diagnostic`
    // requests; advertising it would make pull-capable clients send requests we
    // can only reject with `MethodNotFound`.
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

    let server_capabilities = serde_json::to_value(&capabilities)
        .expect("serialize ServerCapabilities");
    let init_params = connection
        .initialize(server_capabilities)
        .expect("initialize handshake");

    // Extract workspace folders so we can eagerly index all sources under each
    // root. Without eager indexing, cross-file references / rename would only
    // cover files the user has opened.
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
        server.reload_analysis_configuration(&settings);
    }
    if let Err(e) = server.main_loop() {
        eprintln!("rua-lsp: error in main loop: {e}");
    }

    io_threads.join().expect("io threads join");
    eprintln!("rua-lsp: shutdown complete");
}

// --- server state -----------------------------------------------------------

struct Server {
    connection: Connection,
    /// The single source of per-file state (parsed CST + line index + symbol
    /// table + source text). Open buffers are registered via
    /// [`Workspace::add_file`](Workspace::add_file) so unsaved changes are
    /// visible to every query; on-disk files are lazily loaded on demand.
    /// There is no parallel document cache — this is the one owner.
    workspace: Workspace<DiskLoader>,
    /// New analysis pipeline inputs. Feature handlers continue to use the
    /// legacy workspace until their Phase 3/4 migrations are complete.
    analysis_inputs: AnalysisInputs,
}

impl Server {
    fn new(connection: Connection) -> Self {
        Server {
            connection,
            workspace: Workspace::new(DiskLoader),
            analysis_inputs: AnalysisInputs::new(),
        }
    }

    /// Map a document URI to its workspace key path. Falls back to a pseudo-path
    /// built from the raw URI for non-`file:` schemes (e.g. `untitled:` buffers)
    /// so single-file features still work for unsaved, unnamed documents.
    fn doc_key(uri: &Uri) -> PathBuf {
        uri_to_path(uri).unwrap_or_else(|| PathBuf::from(uri.as_str()))
    }

    /// Byte offset of an LSP position within the document at `uri`, using the
    /// document's cached [`LineIndex`]. Lazily loads the analysis if needed.
    fn offset_at(&mut self, uri: &Uri, pos: Position) -> Option<usize> {
        let key = Self::doc_key(uri);
        let a = self.workspace.analysis_of(&key)?;
        Some(
            a.line_index()
                .offset(pos.line as usize, pos.character as usize, a.text()),
        )
    }

    /// Eagerly index every source under each workspace folder so cross-file
    /// references / rename cover the whole project, not just opened files.
    fn index_workspace_folders(&mut self, folders: &[Uri]) {
        for uri in folders {
            if let Some(root) = uri_to_path(uri) {
                let n = self.workspace.index_root(&root);
                eprintln!("rua-lsp: indexed {n} source(s) under {}", root.display());
            }
        }
    }

    fn main_loop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            let msg = match self.connection.receiver.recv() {
                Ok(m) => m,
                Err(_) => return Ok(()), // channel closed — clean shutdown
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
                Message::Response(_resp) => {
                    // We don't send requests yet, so responses are unexpected.
                }
            }
        }
    }

    // --- requests ------------------------------------------------------------

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

    fn handle_formatting(&mut self, req: Request) {
        // Capture the id up front: a request MUST receive a response, so if
        // param extraction fails we still reply with an error (dropping it would
        // leave a spec-conformant client blocked on this id forever).
        let id = req.id.clone();
        let (id, params) = match req.extract::<DocumentFormattingParams>(Formatting::METHOD) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("rua-lsp: bad formatting params: {e:?}");
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
        let edits = self
            .workspace
            .analysis_of(&key)
            .map(|a| format_edits(a.text()))
            .unwrap_or_default();
        let result: Option<Vec<TextEdit>> =
            if edits.is_empty() { None } else { Some(edits) };
        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // --- C3: hover / go-to-definition ---------------------------------------

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

        let result = self.hover_at_uri(uri, pos).map(|(range, detail)| Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!("```rua\n{}\n```", detail),
            }),
            range: Some(range),
        });

        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

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

        let result = self.definition_at_uri(uri, pos).map(|(target_uri, target_range)| {
            GotoDefinitionResponse::Scalar(Location {
                uri: target_uri,
                range: target_range,
            })
        });

        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // --- C4: document symbols / completion ----------------------------------

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
        let key = Self::doc_key(&params.text_document.uri);

        let result = self.workspace.analysis_of(&key).map(|a| {
            let rooted = build_symbol_tree(a.symbols(), a.line_index(), a.text());
            DocumentSymbolResponse::Nested(rooted)
        });

        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    fn handle_completion(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params) = match req.extract::<lsp_types::CompletionParams>(Completion::METHOD) {
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
        let key = Self::doc_key(uri);

        // Completion priority (each `Some(_)` returns ONLY that kind, suppressing
        // globals/keywords like rust-analyzer — even when empty):
        //   1. member (`x.` → fields/methods, cross-file typed)
        //   2. path   (`Type::` / `mod::` → variants / assoc fns / mod items)
        //   3. globals (symbols + keywords) — the fallback.
        let items: Vec<CompletionItem> = match self.offset_at(uri, pos) {
            Some(offset) => {
                if let Some(members) = self.workspace.member_completions(&key, offset) {
                    members.into_iter().map(member_to_item).collect()
                } else if let Some(syms) = self.workspace.path_completions(&key, offset) {
                    symbols_to_items(&syms)
                } else {
                    self.symbol_completions(&key, Some(offset))
                }
            }
            None => self.symbol_completions(&key, None),
        };

        let result = CompletionResponse::Array(items);
        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    /// Global completions (keywords, in-scope locals, symbols, built-in
    /// types/constructors/macros) for the document at `key`. `offset` is the
    /// cursor byte offset used to gather visible locals; `None` skips locals.
    fn symbol_completions(&mut self, key: &Path, offset: Option<usize>) -> Vec<CompletionItem> {
        match self.workspace.analysis_of(key) {
            Some(a) => {
                let locals = offset.map(|o| a.scope_locals(o)).unwrap_or_default();
                global_completions(a.symbols(), &locals)
            }
            None => global_completions(&[], &[]),
        }
    }

    // --- C5: references / rename / prepare-rename ----------------------------

    fn handle_references(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params) = match req.extract::<lsp_types::ReferenceParams>(References::METHOD) {
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

        let result = self.references_at_uri(uri, pos, include_decl);

        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // `lsp_types::Uri` trips `mutable_key_type` (it caches a parsed form), but
    // `WorkspaceEdit::changes` requires exactly `HashMap<Uri, Vec<TextEdit>>`.
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

        let result = self.rename_at_uri(uri, pos, &params.new_name);

        let resp = match result {
            Some(edit) => Response::new_ok(id, edit),
            None => Response::new_err(
                id,
                lsp_server::ErrorCode::InvalidParams as i32,
                "cannot rename at this position".to_string(),
            ),
        };
        let _ = self.connection.sender.send(Message::Response(resp));
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
        let key = Self::doc_key(&params.text_document.uri);
        let pos = params.position;

        let result = self.workspace.analysis_of(&key).and_then(|a| {
            let offset =
                a.line_index()
                    .offset(pos.line as usize, pos.character as usize, a.text());
            let hit = a.ident_at_offset(offset)?;
            // Member accesses (`x.field` / `x.method()`) are jump/hover targets
            // but not yet renamable (cross-impl member rename is v3-d), so don't
            // advertise them as renamable — otherwise the follow-up rename fails.
            // Structural check catches cross-file members too (no type needed).
            if a.is_member_access(offset) {
                return None;
            }
            let _def = a.definition_at(offset)?;
            Some(PrepareRenameResponse::Range(range_from_bytes(
                hit.range,
                a.line_index(),
                a.text(),
            )))
        });

        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

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
        let key = Self::doc_key(&params.text_document.uri);
        let result = self.workspace.analysis_of(&key).map(|analysis| {
            SemanticTokensResult::Tokens(semantic_tokens_for(analysis.text()))
        });
        let response = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(response));
    }

    // --- shared lookup helpers -----------------------------------------------

    /// True when `path` denotes the same file as document `key` under the
    /// workspace's path normalization (canonicalization + `.`/`..` cleanup).
    fn same_file(key: &Path, path: &Path) -> bool {
        normalize_path(key) == normalize_path(path)
    }

    /// URI to report for a workspace path: reuse the request's original `uri`
    /// when it names the same file (so the client sees an identical URI),
    /// otherwise synthesize a `file://` URI.
    fn result_uri(&self, key_uri: &Uri, key: &Path, path: &Path) -> Option<Uri> {
        if Self::same_file(key, path) {
            Some(key_uri.clone())
        } else {
            path_to_uri(path)
        }
    }

    /// Convert a byte range in the file at `path` into an LSP [`Range`], using
    /// that file's cached analysis (lazily loaded).
    fn range_in_file(&mut self, path: &Path, range: (usize, usize)) -> Option<Range> {
        let a = self.workspace.analysis_of(path)?;
        Some(range_from_bytes(range, a.line_index(), a.text()))
    }

    /// Cross-file hover: returns `(highlight_range, detail)` for the ident at
    /// `position`.
    fn hover_at_uri(&mut self, uri: &Uri, pos: Position) -> Option<(Range, String)> {
        let key = Self::doc_key(uri);
        let offset = self.offset_at(uri, pos)?;
        let (hl_range, ident_text) = {
            let a = self.workspace.analysis_of(&key)?;
            let hit = a.ident_at_offset(offset)?;
            (
                range_from_bytes(hit.range, a.line_index(), a.text()),
                hit.text.clone(),
            )
        };
        // Try goto_definition first; fall back to built-in hover for constructors
        // like Some/None/Ok/Err that have no definition site to jump to.
        let detail = self
            .workspace
            .goto_definition(&key, offset)
            .map(|(_, _, _, detail)| detail)
            .or_else(|| builtin_hover_detail(&ident_text))?;
        Some((hl_range, detail))
    }

    /// Cross-file go-to-definition: returns `(target_uri, target_range)`.
    fn definition_at_uri(&mut self, uri: &Uri, pos: Position) -> Option<(Uri, Range)> {
        let key = Self::doc_key(uri);
        let offset = self.offset_at(uri, pos)?;
        let (target_file, target_range, _kind, _detail) =
            self.workspace.goto_definition(&key, offset)?;
        // Builtin methods (Vec/HashMap/String) use a zero-length sentinel span
        // because they have no source definition to jump to — only hover works.
        if target_range.0 == 0 && target_range.1 == 0 {
            return None;
        }
        let target_uri = self.result_uri(uri, &key, &target_file)?;
        let range = self.range_in_file(&target_file, target_range)?;
        Some((target_uri, range))
    }

    /// Cross-file references: returns locations across all indexed files.
    fn references_at_uri(
        &mut self,
        uri: &Uri,
        pos: Position,
        include_decl: bool,
    ) -> Option<Vec<Location>> {
        let key = Self::doc_key(uri);
        let offset = self.offset_at(uri, pos)?;

        let refs = self.workspace.references(&key, offset, include_decl);
        if refs.is_empty() {
            return None;
        }

        let mut locations = Vec::new();
        for (ref_file, ref_range) in refs {
            let Some(ref_uri) = self.result_uri(uri, &key, &ref_file) else {
                continue;
            };
            let Some(range) = self.range_in_file(&ref_file, ref_range) else {
                continue;
            };
            locations.push(Location {
                uri: ref_uri,
                range,
            });
        }
        Some(locations)
    }

    /// Cross-file rename: returns a [`WorkspaceEdit`] with multi-URI changes.
    #[allow(clippy::mutable_key_type)]
    fn rename_at_uri(
        &mut self,
        uri: &Uri,
        pos: Position,
        new_name: &str,
    ) -> Option<WorkspaceEdit> {
        let key = Self::doc_key(uri);
        let offset = self.offset_at(uri, pos)?;

        let edits_by_file = self.workspace.rename_edits(&key, offset, new_name).ok()?;

        let mut changes = std::collections::HashMap::new();
        for (edit_file, edits) in edits_by_file {
            let Some(edit_uri) = self.result_uri(uri, &key, &edit_file) else {
                continue;
            };
            let Some(a) = self.workspace.analysis_of(&edit_file) else {
                continue;
            };
            let text_edits: Vec<TextEdit> = edits
                .into_iter()
                .map(|(start, end, new_text)| TextEdit {
                    range: range_from_bytes((start, end), a.line_index(), a.text()),
                    new_text,
                })
                .collect();
            changes.insert(edit_uri, text_edits);
        }

        Some(WorkspaceEdit::new(changes))
    }

    // --- notifications -------------------------------------------------------

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
                let uri = params.text_document.uri;
                let text = params.text_document.text;
                self.set_document(uri, text);
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
                let uri = params.text_document.uri;
                // Full sync: take the last content change (should be the whole doc).
                if let Some(change) = params.content_changes.last() {
                    self.set_document(uri, change.text.clone());
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
                let uri = params.text_document.uri;
                // Drop the open buffer + its analysis so future queries read the
                // on-disk content again.
                self.workspace.remove_file(&Self::doc_key(&uri));
                // Clear diagnostics for the closed document.
                self.send_diagnostics(&uri, &[]);
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
                self.reload_analysis_configuration(&params.settings);
            }
            _ => {
                // Ignore unknown notifications silently (LSP allows this).
            }
        }
    }

    fn reload_analysis_configuration(&mut self, settings: &serde_json::Value) {
        match self.analysis_inputs.reload_from_settings(settings) {
            Ok(report) => {
                eprintln!(
                    "rua-lsp: loaded {} declaration(s) from {} configured library root(s)",
                    report.file_count, report.configured_root_count
                );
                for warning in report.warnings {
                    eprintln!("rua-lsp: library warning: {warning}");
                }
            }
            Err(error) => {
                eprintln!("rua-lsp: invalid library configuration: {error}");
            }
        }
    }

    /// Register (or replace) the buffer at `uri` in the workspace — the single
    /// owner of parsed state — then publish fresh diagnostics. The analysis is
    /// built exactly once here (via [`Workspace::analysis_of`]); there is no
    /// second per-document cache to keep in sync.
    fn set_document(&mut self, uri: Uri, text: String) {
        let key = Self::doc_key(&uri);
        self.workspace.add_file(&key, &text);
        // Force the parse now so the first query and diagnostics are ready.
        self.workspace.analysis_of(&key);
        self.publish_diagnostics(&uri);
    }

    // --- diagnostics ---------------------------------------------------------

    /// Run the full diagnostic pipeline on the document at `uri` and publish
    /// results.  Reuses the cached [`LineIndex`] from the document's analysis.
    fn publish_diagnostics(&mut self, uri: &Uri) {
        let key = Self::doc_key(uri);
        let lsp_diags: Vec<Diagnostic> = match self.workspace.analysis_of(&key) {
            Some(analysis) => reconciled_diagnostics_for(
                analysis.text(),
                analysis.line_index(),
            ),
            None => return,
        };
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

fn semantic_token_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::PARAMETER,
            SemanticTokenType::METHOD,
            SemanticTokenType::OPERATOR,
        ],
        token_modifiers: vec![SemanticTokenModifier::DECLARATION],
    }
}

fn semantic_tokens_for(source: &str) -> SemanticTokens {
    let file_id = CoreFileId::new(0);
    let mut change = CoreChange::new();
    change.set_file_text(file_id, source);
    let mut host = CoreAnalysisHost::new();
    host.apply_change(change);
    let analysis = host.analysis();
    let line_index = LineIndex::new(source);
    let mut previous_line = 0u32;
    let mut previous_start = 0u32;
    let mut data = Vec::new();

    for token in analysis.semantic_tokens(file_id) {
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
        let token_type = match token.kind() {
            SemanticTokenKind::ClosureParameter => 0,
            SemanticTokenKind::Method => 1,
            SemanticTokenKind::RangeOperator => 2,
        };
        data.push(LspSemanticToken {
            delta_line,
            delta_start,
            length: source[start..end].encode_utf16().count() as u32,
            token_type,
            token_modifiers_bitset: u32::from(token.is_declaration()),
        });
        previous_line = line;
        previous_start = column;
    }

    SemanticTokens {
        result_id: None,
        data,
    }
}

fn reconciled_diagnostics_for(
    source: &str,
    line_index: &LineIndex,
) -> Vec<Diagnostic> {
    let file_id = CoreFileId::new(0);
    let mut change = CoreChange::new();
    change.set_file_text(file_id, source);
    let mut host = CoreAnalysisHost::new();
    host.apply_change(change);
    let fast = host.analysis().diagnostics(file_id);

    let (compiler_raw, _) = ruac::check_diags(source);
    let compiler: Vec<_> = compiler_raw
        .iter()
        .map(|diagnostic| {
            CoreDiagnostic::new(
                file_id,
                CoreTextRange::new(
                    diagnostic.start as u32,
                    (diagnostic.start + diagnostic.len) as u32,
                ),
                diagnostic.msg.clone(),
                DiagnosticOrigin::Compiler,
            )
        })
        .collect();
    let reconciled = reconcile_diagnostics(fast, compiler);

    if reconciled
        .first()
        .is_some_and(|diagnostic| diagnostic.origin() == DiagnosticOrigin::Compiler)
    {
        compiler_raw
            .iter()
            .map(|diagnostic| diag_to_lsp(diagnostic, line_index, source))
            .collect()
    } else {
        reconciled
            .iter()
            .map(|diagnostic| core_diag_to_lsp(diagnostic, line_index, source))
            .collect()
    }
}

fn core_diag_to_lsp(
    diagnostic: &CoreDiagnostic,
    line_index: &LineIndex,
    source: &str,
) -> Diagnostic {
    let range = diagnostic.range();
    let (start_line, start_column) = line_index.line_col(range.start() as usize, source);
    let (end_line, end_column) = line_index.line_col(range.end() as usize, source);
    Diagnostic {
        range: Range {
            start: Position::new(start_line as u32, start_column as u32),
            end: Position::new(end_line as u32, end_column as u32),
        },
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some(match diagnostic.origin() {
            DiagnosticOrigin::FastAnalysis => "rua-analysis",
            DiagnosticOrigin::Compiler => "ruac",
        }
        .to_string()),
        message: diagnostic.message().to_string(),
        ..Diagnostic::default()
    }
}

// --- Diag → LSP Diagnostic conversion ---------------------------------------

/// Convert a ruac [`Diag`](ruac::diag::Diag) into an LSP
/// [`Diagnostic`]. Uses the provided [`LineIndex`] to convert byte offsets into
/// `(line, UTF-16 column)` positions.
///
/// Fallback strategy for diagnostics without precise spans:
///   - `len > 0` → precise byte range via `LineIndex::line_col` (handles a span
///     that legitimately starts at byte offset 0, e.g. the first token)
///   - `len == 0 && line > 0` → whole-line range (from line start to line end)
///   - `len == 0 && line == 0` (bare) → position (0,0)–(0,0), still visible
fn diag_to_lsp(d: &ruac::diag::Diag, li: &LineIndex, src: &str) -> Diagnostic {
    let range = if d.len > 0 {
        // Precise byte-offset range.
        let end_offset = (d.start + d.len).min(src.len());
        let (start_line, start_col) = li.line_col(d.start, src);
        let (end_line, end_col) = li.line_col(end_offset, src);
        Range {
            start: Position::new(start_line as u32, start_col as u32),
            end: Position::new(end_line as u32, end_col as u32),
        }
    } else if d.line > 0 {
        // Whole-line fallback for diagnostics that carry a line but no offset.
        let line = (d.line - 1).min(li.line_count() - 1); // 1-based → 0-based
        let line_start = li.line_start(line).unwrap_or(0);
        let line_end = li
            .line_start(line + 1)
            .map(|off| {
                // Strip trailing line break.
                let b = src.as_bytes();
                let mut e = off;
                if e > line_start && b.get(e - 1) == Some(&b'\n') {
                    e -= 1;
                    if e > line_start && b.get(e - 1) == Some(&b'\r') {
                        e -= 1;
                    }
                }
                e
            })
            .unwrap_or(src.len());
        let (sl, sc) = li.line_col(line_start, src);
        let (el, ec) = li.line_col(line_end, src);
        Range {
            start: Position::new(sl as u32, sc as u32),
            end: Position::new(el as u32, ec as u32),
        }
    } else {
        // Bare diagnostic: place at the very start of the file.
        Range {
            start: Position::new(0, 0),
            end: Position::new(0, 0),
        }
    };

    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("ruac".into()),
        message: d.msg.clone(),
        ..Diagnostic::default()
    }
}

// --- Formatting helpers -------------------------------------------------------

/// Format `src` using the Rua formatter and return the edit needed to apply the
/// result. Returns an empty vec when the document is already formatted or when
/// the formatter cannot parse the input (it returns the source unchanged in that
/// case, so `formatted == src`).
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

/// Return a [`Range`] covering the entire document, from (0,0) to the end of
/// the last line. Column counts use UTF-16 code units per the LSP spec.
fn whole_document_range(src: &str) -> Range {
    let li = LineIndex::new(src);
    let (end_line, end_col) = li.line_col(src.len(), src);
    Range {
        start: Position::new(0, 0),
        end: Position::new(end_line as u32, end_col as u32),
    }
}

// --- URI / Path conversion --------------------------------------------------

/// Convert an LSP `file://` [`Uri`] to a filesystem [`PathBuf`].
fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let s = uri.as_str();
    // Strip `file://` prefix (handles `file:///path` and `file://path`).
    let path_str = s.strip_prefix("file://")?;
    // On Unix, paths start with `/`; remove leading `/` if double-slash.
    let path_str = if let Some(rest) = path_str.strip_prefix("//") {
        rest
    } else {
        path_str
    };
    // Percent-decode the path (simple: just decode %xx).
    let decoded = percent_decode(path_str);
    Some(PathBuf::from(decoded))
}

/// Convert a filesystem [`PathBuf`] to an LSP `file://` [`Uri`].
fn path_to_uri(path: &Path) -> Option<Uri> {
    let s = path.to_string_lossy();
    let encoded = percent_encode(&s);
    let uri_str = format!("file://{}", encoded);
    uri_str.parse().ok()
}

/// Percent-decoding: `%20` → ` `, `%E4%B8%AD` → `中`, etc.
///
/// Decodes into a byte buffer first so multi-byte UTF-8 sequences (each byte
/// percent-encoded separately) reassemble correctly, then interprets the whole
/// buffer as UTF-8. A stray `%` that is not followed by two hex digits is kept
/// verbatim.
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

/// Simple percent-encoding for file paths (only encode special chars).
fn percent_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            ' ' => result.push_str("%20"),
            // Keep most chars as-is for file paths.
            _ => result.push(c),
        }
    }
    result
}

// --- C3/C4 helpers -----------------------------------------------------------

/// Convert a byte-offset pair into an LSP [`Range`].
fn range_from_bytes((start, end): (usize, usize), li: &LineIndex, src: &str) -> Range {
    let start = start.min(src.len());
    let end = end.min(src.len());
    let (sl, sc) = li.line_col(start, src);
    let (el, ec) = li.line_col(end, src);
    Range {
        start: Position::new(sl as u32, sc as u32),
        end: Position::new(el as u32, ec as u32),
    }
}

/// Map a syntax-level [`SymbolKind`] to LSP's [`LspSymbolKind`].
fn to_lsp_symbol_kind(k: SymbolKind) -> LspSymbolKind {
    match k {
        SymbolKind::Function | SymbolKind::ExternFn => LspSymbolKind::FUNCTION,
        SymbolKind::Struct => LspSymbolKind::STRUCT,
        SymbolKind::Enum => LspSymbolKind::ENUM,
        SymbolKind::Trait => LspSymbolKind::INTERFACE,
        SymbolKind::Impl => LspSymbolKind::OBJECT,
        SymbolKind::Method => LspSymbolKind::METHOD,
        SymbolKind::Field => LspSymbolKind::FIELD,
        SymbolKind::Variant => LspSymbolKind::ENUM_MEMBER,
        SymbolKind::Module => LspSymbolKind::MODULE,
    }
}

/// Map a syntax-level [`SymbolKind`] to LSP's [`CompletionItemKind`].
fn to_completion_kind(k: SymbolKind) -> CompletionItemKind {
    match k {
        SymbolKind::Function | SymbolKind::ExternFn => CompletionItemKind::FUNCTION,
        SymbolKind::Struct => CompletionItemKind::STRUCT,
        SymbolKind::Enum => CompletionItemKind::ENUM,
        SymbolKind::Trait => CompletionItemKind::INTERFACE,
        SymbolKind::Impl => CompletionItemKind::CLASS,
        SymbolKind::Method => CompletionItemKind::METHOD,
        SymbolKind::Field => CompletionItemKind::FIELD,
        SymbolKind::Variant => CompletionItemKind::ENUM_MEMBER,
        SymbolKind::Module => CompletionItemKind::MODULE,
    }
}

/// Map a type-checked member into an LSP completion item (field or method).
///
/// Methods are inserted as a snippet so accepting `go` yields `go()` (no args)
/// or `go($0)` (args) with the cursor placed for the argument list, saving the
/// user from typing the parentheses.
fn member_to_item(m: CompletionMember) -> CompletionItem {
    match m.kind {
        MemberKind::Field => CompletionItem {
            label: m.name,
            kind: Some(CompletionItemKind::FIELD),
            detail: Some(m.detail),
            ..Default::default()
        },
        MemberKind::Method => {
            let snippet = if method_detail_has_args(&m.detail) {
                format!("{}($0)", m.name)
            } else {
                format!("{}()$0", m.name)
            };
            CompletionItem {
                label: m.name,
                kind: Some(CompletionItemKind::METHOD),
                detail: Some(m.detail),
                insert_text: Some(snippet),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            }
        }
    }
}

/// Whether a method signature detail (e.g. `fn go(&self, n: i64) -> i64`)
/// declares any caller-supplied argument beyond the `self` receiver.
fn method_detail_has_args(detail: &str) -> bool {
    let open = match detail.find('(') {
        Some(i) => i,
        None => return false,
    };
    let close = match detail[open + 1..].find(')') {
        Some(i) => open + 1 + i,
        None => return false,
    };
    detail[open + 1..close]
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .any(|p| !matches!(p, "self" | "&self" | "&mut self"))
}

/// Build an LSP completion item from a definition [`Symbol`], surfacing its doc
/// comment (if any) as markdown documentation.
fn symbol_to_item(sym: &Symbol) -> CompletionItem {
    CompletionItem {
        label: sym.name.clone(),
        kind: Some(to_completion_kind(sym.kind)),
        detail: (!sym.detail.is_empty()).then(|| sym.detail.clone()),
        documentation: symbol_documentation(sym),
        ..Default::default()
    }
}

/// Wrap a symbol's doc text as LSP markdown documentation, or `None` when the
/// symbol is undocumented.
fn symbol_documentation(sym: &Symbol) -> Option<Documentation> {
    (!sym.doc.is_empty()).then(|| {
        Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: sym.doc.clone(),
        })
    })
}

/// Map a symbol list into deduplicated completion items (path-context helper).
fn symbols_to_items(syms: &[Symbol]) -> Vec<CompletionItem> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    syms.iter()
        .filter(|s| seen.insert(s.name.as_str()))
        .map(symbol_to_item)
        .collect()
}

/// Keyword-like literals offered in completion in addition to the canonical
/// language keywords (`rua_syntax::nameres::RUA_KEYWORDS`). These are not
/// valid rename targets, so they live only here, not in the shared list.
const KEYWORD_LITERALS: &[&str] = &["self", "true", "false"];

/// All identifiers offered as `KEYWORD` completions: the shared language
/// keyword list plus the keyword-like literals.
fn completion_keywords() -> impl Iterator<Item = &'static str> {
    rua_syntax::nameres::RUA_KEYWORDS
        .iter()
        .copied()
        .chain(KEYWORD_LITERALS.iter().copied())
}

/// Build a hierarchical [`DocumentSymbol`] tree from a flat [`Symbol`] list.
// `DocumentSymbol::deprecated` is itself deprecated (superseded by `tags`), but
// the struct has no `Default`, so we must name the field; silence the lint.
#[allow(deprecated)]
fn build_symbol_tree(syms: &[Symbol], li: &LineIndex, src: &str) -> Vec<DocumentSymbol> {
    let mut roots: Vec<DocumentSymbol> = Vec::new();

    for sym in syms {
        let ds = DocumentSymbol {
            name: sym.name.clone(),
            detail: if sym.detail.is_empty() {
                None
            } else {
                Some(sym.detail.clone())
            },
            kind: to_lsp_symbol_kind(sym.kind),
            range: range_from_bytes(sym.full_range, li, src),
            selection_range: range_from_bytes(sym.name_range, li, src),
            children: None,
            tags: None,
            deprecated: None,
        };

        if sym.container.is_empty() {
            roots.push(ds);
        } else {
            let parent_name = sym.container.last().unwrap();
            if let Some(parent) = find_parent_mut(&mut roots, parent_name) {
                parent.children.get_or_insert_with(Vec::new).push(ds);
            } else {
                // Orphan: push as root (resilience against partial trees).
                roots.push(ds);
            }
        }
    }
    roots
}

/// Recursively search for a [`DocumentSymbol`] with the given `name`.
fn find_parent_mut<'a>(
    nodes: &'a mut [DocumentSymbol],
    name: &str,
) -> Option<&'a mut DocumentSymbol> {
    for node in nodes.iter_mut() {
        if node.name == name {
            return Some(node);
        }
        if let Some(ref mut children) = node.children
            && let Some(found) = find_parent_mut(children, name)
        {
            return Some(found);
        }
    }
    None
}

/// Build the completion item list from a source string (convenience wrapper
/// that parses internally; useful for tests).  Handlers should prefer
/// [`completions_from_symbols`] with the cached analysis.
#[cfg(test)]
fn completions_for(src: &str) -> Vec<CompletionItem> {
    let analysis = rua_syntax::analysis::Analysis::new(src);
    global_completions(analysis.symbols(), &[])
}

/// Full decision-mirror for member/global completion, testable without a live
/// LSP connection. Cursor position is a byte offset into `src`.
#[cfg(test)]
fn completion_items_at(src: &str, offset: usize) -> Vec<CompletionItem> {
    let a = rua_syntax::analysis::Analysis::new(src);
    if let Some(members) = a.member_completions(offset) {
        members.into_iter().map(member_to_item).collect()
    } else if let Some(syms) = a.path_completions(offset) {
        symbols_to_items(&syms)
    } else {
        global_completions(a.symbols(), &a.scope_locals(offset))
    }
}

/// Built-in type names offered in global completion.
const BUILTIN_TYPES: &[&str] = &[
    "i64", "f64", "bool", "String", "str", "Vec", "HashMap", "Option", "Result", "Box",
];

/// Built-in constructors / variants offered as value completions.
const BUILTIN_VALUES: &[&str] = &["Some", "None", "Ok", "Err"];

/// Built-in macros offered in global completion. Inserted as `name!(...)`
/// (or `vec![...]`), with the cursor placed inside the delimiter.
const BUILTIN_MACROS: &[&str] = &[
    "println",
    "print",
    "format",
    "vec",
    "panic",
    "assert",
    "assert_eq",
    "assert_ne",
    "unreachable",
    "unimplemented",
    "todo",
    "dbg",
    "include_str",
    "include_bytes",
];

/// Build the global (non-member, non-path) completion set: keywords, in-scope
/// locals, user-defined symbols, built-in types, built-in constructors, and
/// built-in macros — deduplicated by name (first writer wins, in that priority
/// order). LSP clients prefix-filter this list against the typed text.
fn global_completions(syms: &[Symbol], locals: &[LocalCompletion]) -> Vec<CompletionItem> {
    let mut items = keyword_items();

    let mut seen: std::collections::HashSet<String> =
        completion_keywords().map(|kw| kw.to_string()).collect();

    // In-scope locals first (highest relevance for the cursor position).
    for l in locals {
        if seen.insert(l.name.clone()) {
            items.push(CompletionItem {
                label: l.name.clone(),
                kind: Some(CompletionItemKind::VARIABLE),
                detail: Some(l.detail.clone()),
                ..Default::default()
            });
        }
    }

    // User-defined top-level symbols.
    for sym in syms {
        if seen.insert(sym.name.clone()) {
            items.push(symbol_to_item(sym));
        }
    }

    // Built-in types.
    for ty in BUILTIN_TYPES {
        if seen.insert((*ty).to_string()) {
            items.push(CompletionItem {
                label: (*ty).to_string(),
                kind: Some(CompletionItemKind::STRUCT),
                detail: Some(format!("{ty}  (built-in type)")),
                ..Default::default()
            });
        }
    }

    // Built-in constructors / variants (`Some`, `None`, `Ok`, `Err`).
    for v in BUILTIN_VALUES {
        if seen.insert((*v).to_string()) {
            items.push(CompletionItem {
                label: (*v).to_string(),
                kind: Some(CompletionItemKind::ENUM_MEMBER),
                detail: builtin_hover_detail(v),
                ..Default::default()
            });
        }
    }

    // Built-in macros, inserted as a snippet with the delimiter pre-typed.
    for m in BUILTIN_MACROS {
        if seen.insert((*m).to_string()) {
            let snippet = if *m == "vec" {
                format!("{m}![$0]")
            } else {
                format!("{m}!($0)")
            };
            items.push(CompletionItem {
                label: format!("{m}!"),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: builtin_hover_detail(m),
                insert_text: Some(snippet),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                // Match the client's typed prefix (`prin` → `println!`) against
                // the bare name, since the label carries a trailing `!`.
                filter_text: Some((*m).to_string()),
                ..Default::default()
            });
        }
    }

    items
}


/// The full set of Rua keyword completion items.
fn keyword_items() -> Vec<CompletionItem> {
    completion_keywords()
        .map(|kw| CompletionItem {
            label: kw.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some(format!("keyword {}", kw)),
            ..Default::default()
        })
        .collect()
}

/// Hover detail text for built-in constructors, macros, and literals that
/// have no source definition to jump to but still benefit from a tooltip.
fn builtin_hover_detail(name: &str) -> Option<String> {
    let detail = match name {
        "Some" => "Some(value) -> Option<T>  (built-in constructor)",
        "None" => "None: Option<T>  (built-in variant)",
        "Ok" => "Ok(value) -> Result<T, E>  (built-in constructor)",
        "Err" => "Err(error) -> Result<T, E>  (built-in constructor)",
        "true" | "false" => "bool  (built-in literal)",
        "self" => "self  (current instance)",
        "vec" => "vec![...] -> Vec<T>  (built-in macro)",
        "println" | "print" => "println!(...) / print!(...)  (built-in macro)",
        "format" => "format!(...) -> String  (built-in macro)",
        "panic" => "panic!(msg) -> !  (built-in macro)",
        "assert" => "assert!(condition)  (built-in macro)",
        "assert_eq" => "assert_eq!(left, right)  (built-in macro)",
        "assert_ne" => "assert_ne!(left, right)  (built-in macro)",
        "unreachable" => "unreachable!() -> !  (built-in macro)",
        "unimplemented" => "unimplemented!() -> !  (built-in macro)",
        "todo" => "todo!() -> !  (built-in macro)",
        "dbg" => "dbg!(expr)  (built-in debug macro)",
        "include_str" => "include_str!(path) -> &str  (built-in macro)",
        "include_bytes" => "include_bytes!(path) -> &[u8]  (built-in macro)",
        _ => return None,
    };
    Some(detail.to_string())
}

// --- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        builtin_hover_detail, completion_items_at, completions_for, diag_to_lsp, format_edits,
        member_to_item, percent_decode, range_from_bytes, semantic_tokens_for,
        reconciled_diagnostics_for, to_lsp_symbol_kind, uri_to_path, whole_document_range, Server,
    };
    use lsp_types::{CompletionItemKind, Documentation, InsertTextFormat, Position};
    use std::path::PathBuf;
    use ruac::diag::Diag;
    use rua_syntax::analysis::{CompletionMember, MemberKind};
    use rua_syntax::symbols::{SymbolKind, collect_symbols};
    use rua_syntax::{LineIndex, ast::SourceFile, parse_source_file};

    fn range_of(d: &Diag, src: &str) -> (Position, Position) {
        let li = LineIndex::new(src);
        let r = diag_to_lsp(d, &li, src).range;
        (r.start, r.end)
    }

    // --- percent decoding / uri tests -------------------------------------

    #[test]
    fn percent_decode_ascii_space() {
        assert_eq!(percent_decode("a%20b"), "a b");
    }

    #[test]
    fn percent_decode_multibyte_utf8() {
        // U+4E2D (中) is E4 B8 AD in UTF-8 — three separately-encoded bytes
        // must reassemble into one char, not three Latin-1 chars.
        assert_eq!(percent_decode("%E4%B8%AD"), "中");
    }

    #[test]
    fn percent_decode_stray_percent_kept() {
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn uri_to_path_decodes_unicode() {
        let uri: lsp_types::Uri = "file:///proj/%E4%B8%AD/main.rua".parse().unwrap();
        assert_eq!(uri_to_path(&uri), Some(PathBuf::from("/proj/中/main.rua")));
    }

    // --- diag_to_lsp tests -------------------------------------------------

    #[test]
    fn precise_span_maps_to_exact_range() {
        let src = "let x = 1\n";
        // `x` at byte 4, length 1.
        let d = Diag::new(0, 4, 1, 1, "oops".into());
        let (start, end) = range_of(&d, src);
        assert_eq!(start, Position::new(0, 4));
        assert_eq!(end, Position::new(0, 5));
    }

    #[test]
    fn span_starting_at_offset_zero_is_precise() {
        // Regression: a real span at byte 0 must NOT be demoted to whole-line.
        let src = "abcdef";
        let d = Diag::new(0, 0, 3, 1, "head".into());
        let (start, end) = range_of(&d, src);
        assert_eq!(start, Position::new(0, 0));
        assert_eq!(end, Position::new(0, 3));
    }

    #[test]
    fn multi_line_span_spans_lines() {
        let src = "ab\ncdef";
        // Covers "b\ncd": bytes 1..5.
        let d = Diag::new(0, 1, 4, 1, "wide".into());
        let (start, end) = range_of(&d, src);
        assert_eq!(start, Position::new(0, 1));
        assert_eq!(end, Position::new(1, 2));
    }

    #[test]
    fn line_only_diag_highlights_whole_line() {
        let src = "ab\ncd\nef";
        // No byte span, but a 1-based line 2 → whole 0-based line 1 ("cd").
        let d = Diag::new(0, 0, 0, 2, "line".into());
        let (start, end) = range_of(&d, src);
        assert_eq!(start, Position::new(1, 0));
        assert_eq!(end, Position::new(1, 2));
    }

    #[test]
    fn bare_diag_points_at_file_start() {
        let src = "whatever\n";
        let d = Diag::bare("no location".into());
        let (start, end) = range_of(&d, src);
        assert_eq!(start, Position::new(0, 0));
        assert_eq!(end, Position::new(0, 0));
    }

    #[test]
    fn span_end_is_clamped_to_source_len() {
        let src = "xy";
        // len overshoots the end of the source; end must clamp, not panic.
        let d = Diag::new(0, 1, 100, 1, "overshoot".into());
        let (_start, end) = range_of(&d, src);
        assert_eq!(end, Position::new(0, 2));
    }

    // --- format_edits tests -------------------------------------------------

    #[test]
    fn format_edits_noop_for_already_formatted() {
        let src = "fn main() {}\n";
        let formatted = rua_syntax::format::format_str(src);
        assert_eq!(formatted, src, "test fixture must be already formatted");
        assert!(format_edits(src).is_empty());
    }

    #[test]
    fn format_edits_noop_for_parse_error() {
        // Malformed input: formatter returns src unchanged → no edits.
        let src = "fn {";
        let edits = format_edits(src);
        assert!(edits.is_empty(), "parse-error input must produce no edits");
    }

    #[test]
    fn format_edits_produces_whole_document_edit() {
        // Extra whitespace that the formatter will strip.
        let src = "fn main()   {}\n";
        let edits = format_edits(src);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].range.start, Position::new(0, 0));
        let expected_end = whole_document_range(src).end;
        assert_eq!(edits[0].range.end, expected_end);
        assert_eq!(edits[0].new_text, rua_syntax::format::format_str(src));
    }

    #[test]
    fn whole_document_range_empty_file() {
        let r = whole_document_range("");
        assert_eq!(r.start, Position::new(0, 0));
        assert_eq!(r.end, Position::new(0, 0));
    }

    #[test]
    fn whole_document_range_single_line() {
        let r = whole_document_range("hello");
        assert_eq!(r.start, Position::new(0, 0));
        assert_eq!(r.end, Position::new(0, 5));
    }

    #[test]
    fn whole_document_range_multi_line() {
        let r = whole_document_range("ab\ncde\nf");
        assert_eq!(r.start, Position::new(0, 0));
        // Last line has 1 char "f" → col 1 in UTF-16.
        assert_eq!(r.end, Position::new(2, 1));
    }

    // --- C3/C4 tests --------------------------------------------------------

    fn src_file(src: &str) -> SourceFile {
        parse_source_file(src).tree
    }

    #[test]
    fn hover_finds_function_detail() {
        let src = "fn add_one(x: i64) -> i64 { x + 1 }";
        let file = src_file(src);
        let syms = collect_symbols(&file);
        let sym = syms.iter().find(|s| s.name == "add_one").unwrap();
        assert_eq!(sym.kind, SymbolKind::Function);
        assert_eq!(sym.detail, "fn add_one(x: i64) -> i64");
    }

    #[test]
    fn hover_none_for_whitespace_position() {
        let src = "fn f() {}";
        let file = src_file(src);
        let li = LineIndex::new(src);
        // Byte 2 is the space between "fn" and "f"
        let offset = li.offset(0, 2, src);
        let hit = rua_syntax::symbols::ident_at_offset(&file, offset);
        assert!(hit.is_none());
    }

    #[test]
    fn hover_none_for_keyword_position() {
        let src = "fn f() {}";
        let file = src_file(src);
        // Byte 0 is "fn" keyword
        let hit = rua_syntax::symbols::ident_at_offset(&file, 0);
        assert!(hit.is_none());
    }

    #[test]
    fn goto_def_jumps_to_name_range() {
        let src = "fn add_one(x: i64) -> i64 { x + 1 }";
        let li = LineIndex::new(src);
        let file = src_file(src);
        let syms = collect_symbols(&file);
        let sym = &syms[0];
        let range = range_from_bytes(sym.name_range, &li, src);
        // "add_one" starts at byte 3
        assert_eq!(range.start, Position::new(0, 3));
        assert_eq!(range.end, Position::new(0, 10));
    }

    #[test]
    fn goto_def_multiple_same_name() {
        let src = "fn f() {}\nfn f(x: i64) {}";
        let syms = collect_symbols(&src_file(src));
        let f_syms: Vec<_> = syms.iter().filter(|s| s.name == "f").collect();
        assert_eq!(f_syms.len(), 2);
    }

    #[test]
    fn range_from_bytes_single_line() {
        let src = "hello\nworld";
        let li = LineIndex::new(src);
        let range = range_from_bytes((0, 5), &li, src);
        assert_eq!(range.start, Position::new(0, 0));
        assert_eq!(range.end, Position::new(0, 5));
    }

    #[test]
    fn range_from_bytes_multiline() {
        let src = "ab\ncd";
        let li = LineIndex::new(src);
        // Bytes 1..4 cover "b\nc": start at (0,1), end after "c" at (1,1).
        let range = range_from_bytes((1, 4), &li, src);
        assert_eq!(range.start, Position::new(0, 1));
        assert_eq!(range.end, Position::new(1, 1));
    }

    #[test]
    fn range_from_bytes_clamped() {
        let src = "xy";
        let li = LineIndex::new(src);
        // end beyond source length
        let range = range_from_bytes((0, 999), &li, src);
        assert_eq!(range.end, Position::new(0, 2));
    }

    // --- C4 tests -----------------------------------------------------------

    #[test]
    fn document_symbols_flat_to_tree() {
        let src = "fn a() {}\nstruct B { x: i64 }";
        let file = src_file(src);
        let syms = collect_symbols(&file);
        let li = LineIndex::new(src);
        let tree = super::build_symbol_tree(&syms, &li, src);
        assert_eq!(tree.len(), 2); // a + B
        // B should have field x as child
        let b = tree.iter().find(|ds| ds.name == "B").unwrap();
        let children = b.children.as_ref().unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "x");
    }

    #[test]
    fn document_symbols_empty_source() {
        let src = "";
        let file = src_file(src);
        let syms = collect_symbols(&file);
        assert!(syms.is_empty());
        let li = LineIndex::new(src);
        let tree = super::build_symbol_tree(&syms, &li, src);
        assert!(tree.is_empty());
    }

    #[test]
    fn completion_includes_function_names() {
        let src = "fn hello_world() {}";
        let items = completions_for(src);
        assert!(items.iter().any(|i| i.label == "hello_world"));
        // Keywords are also present
        assert!(items.iter().any(|i| i.label == "fn"));
    }

    #[test]
    fn completion_includes_keywords() {
        let items = completions_for("");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"fn"));
        assert!(labels.contains(&"struct"));
        assert!(labels.contains(&"let"));
        assert!(labels.contains(&"match"));
        assert!(labels.contains(&"return"));
    }

    #[test]
    fn completion_no_duplicates() {
        // Two fns with same name
        let src = "fn f() {}\nfn f(x: i64) {}";
        let items = completions_for(src);
        let f_count = items.iter().filter(|i| i.label == "f").count();
        assert_eq!(f_count, 1, "duplicate symbol names must be deduplicated");
    }

    #[test]
    fn symbol_kind_mapping_covers_all_variants() {
        use rua_syntax::symbols::SymbolKind::*;
        for k in &[Function, Struct, Enum, Trait, Impl, Method, Field, Variant, Module, ExternFn]
        {
            let _ = to_lsp_symbol_kind(*k);
            let _ = super::to_completion_kind(*k);
        }
    }

    #[test]
    fn completions_for_empty_returns_keywords() {
        let items = completions_for("");
        // Should at minimum include Rua keywords
        assert!(!items.is_empty());
        assert!(items.iter().any(|i| i.label == "fn"));
    }

    // --- C3: member completion integration tests -----------------------------

    #[test]
    fn completion_member_context_returns_only_members() {
        let src = "struct P { x: i64 }\nimpl P { fn go(&self) -> i64 { 0 } }\nfn main() { let p = P { x: 1 }; p. }";
        let items = completion_items_at(src, src.rfind('.').unwrap() + 1);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"x") && labels.contains(&"go"));
        assert!(!labels.contains(&"fn"), "member context must exclude keywords");
        assert_eq!(
            items.iter().find(|i| i.label == "x").unwrap().kind,
            Some(CompletionItemKind::FIELD)
        );
        assert_eq!(
            items.iter().find(|i| i.label == "go").unwrap().kind,
            Some(CompletionItemKind::METHOD)
        );
    }

    #[test]
    fn completion_non_member_context_returns_globals() {
        let src = "struct P { x: i64 }\nfn helper() {}\nfn main() {  }";
        let off = src.rfind("  }").unwrap() + 1; // inside empty main body
        let items = completion_items_at(src, off);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"fn"));     // keyword present
        assert!(labels.contains(&"P"));      // symbol present
        assert!(labels.contains(&"helper"));
    }

    #[test]
    fn completion_offered_for_partial_ident_before_next_statement() {
        // Typing a bare partial identifier (`fin`) on its own line — even when
        // the following `if` makes the doc a parse error — must still offer
        // globals (the fn `find`, the local `nums`, keywords). Regression for
        // "no completion while typing `fin`".
        let src = "fn find(v: Vec<i64>) -> i64 { 0 }\nfn main() {\n    let nums = vec![10];\n    fin\n    if let Some(idx) = find(nums) {}\n}\n";
        let off = src.find("    fin\n").unwrap() + "    fin".len();
        let items = completion_items_at(src, off);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"find"), "fn find should be offered: {labels:?}");
        assert!(labels.contains(&"nums"), "local nums should be offered: {labels:?}");
    }

    #[test]
    fn completion_globals_include_builtin_types_and_macros() {
        let src = "fn main() {  }";
        let off = src.rfind("  }").unwrap() + 1;
        let items = completion_items_at(src, off);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        // Built-in types.
        assert!(labels.contains(&"Vec"), "Vec type: {labels:?}");
        assert!(labels.contains(&"HashMap"));
        assert!(labels.contains(&"String"));
        // Built-in macros carry a trailing `!` in the label.
        assert!(labels.contains(&"println!"), "println! macro: {labels:?}");
        assert!(labels.contains(&"vec!"));
        // The macro insert-text is a snippet with the delimiter pre-typed.
        let vec_macro = items.iter().find(|i| i.label == "vec!").unwrap();
        assert_eq!(vec_macro.insert_text.as_deref(), Some("vec![$0]"));
        assert_eq!(vec_macro.insert_text_format, Some(InsertTextFormat::SNIPPET));
        assert_eq!(vec_macro.filter_text.as_deref(), Some("vec"));
    }

    #[test]
    fn completion_globals_include_in_scope_locals() {
        // `let mut stack = ...; <cursor>` — `stack` must be offered, typed.
        let src = "fn main() { let mut stack = vec![1]; \n }";
        let off = src.rfind('\n').unwrap(); // after the let statement
        let items = completion_items_at(src, off);
        let stack = items
            .iter()
            .find(|i| i.label == "stack")
            .expect("in-scope local `stack` should be offered");
        assert_eq!(stack.kind, Some(CompletionItemKind::VARIABLE));
        // Detail is the compiler's inferred type text.
        assert!(stack.detail.as_deref().unwrap().contains("stack"));
    }

    #[test]
    fn closure_iterator_ide_completion_includes_typed_parameter() {
        let src = concat!(
            "fn main() {\n",
            "  let values = vec![1, 2, 3];\n",
            "  let count = values.iter().map(|item| item + 1).count();\n",
            "}\n",
        );
        let offset = src.rfind("item +").unwrap();
        let items = completion_items_at(src, offset);
        let item = items.iter().find(|item| item.label == "item").unwrap();
        assert_eq!(item.detail.as_deref(), Some("closure parameter item: i64"));
    }

    #[test]
    fn closure_iterator_ide_semantic_tokens_cover_params_methods_and_range() {
        let src = "fn main() { (0..3).map(|value| value + 1).filter(|item| item > 1).count(); }";
        let tokens = semantic_tokens_for(src);
        let line_index = LineIndex::new(src);
        let mut line = 0u32;
        let mut column = 0u32;
        let mut decoded = Vec::new();
        for token in tokens.data {
            line += token.delta_line;
            column = if token.delta_line == 0 {
                column + token.delta_start
            } else {
                token.delta_start
            };
            let start = line_index.offset(line as usize, column as usize, src);
            let end = start + token.length as usize;
            decoded.push((src[start..end].to_string(), token.token_type, token.token_modifiers_bitset));
        }
        for method in ["map", "filter", "count"] {
            assert!(decoded.iter().any(|token| token.0 == method && token.1 == 1));
        }
        assert!(decoded.iter().any(|token| token.0 == ".." && token.1 == 2));
        for parameter in ["value", "item"] {
            assert!(decoded.iter().any(|token| {
                token.0 == parameter && token.1 == 0 && token.2 == 1
            }));
            assert!(decoded.iter().any(|token| {
                token.0 == parameter && token.1 == 0 && token.2 == 0
            }));
        }
    }

    #[test]
    fn closure_iterator_ide_diagnostics_publish_compiler_parity_once() {
        let src = concat!(
            "fn main() {\n",
            "  let values = vec![1, 2, 3];\n",
            "  let count = values.iter().filter(|value| value + 1).count();\n",
            "}\n",
        );
        let line_index = LineIndex::new(src);
        let diagnostics = reconciled_diagnostics_for(src, &line_index);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].source.as_deref(), Some("ruac"));
        assert_eq!(
            diagnostics[0].message,
            "iterator filter predicate must be `bool`, found `i64`"
        );
        assert_eq!(diagnostics[0].range.start.line, 2);
    }

    #[test]
    fn closure_iterator_ide_lsp_hover_goto_references_and_rename() {
        let src = concat!(
            "fn main() {\n",
            "  let values = vec![1, 2, 3];\n",
            "  let count = values.iter().map(|item| item + 1).count();\n",
            "}\n",
        );
        let uri: lsp_types::Uri = "file:///tmp/rua-closure-iterator-ide.rua".parse().unwrap();
        let (server_connection, _client_connection) = lsp_server::Connection::memory();
        let mut server = Server::new(server_connection);
        server.set_document(uri.clone(), src.to_string());
        let use_offset = src.rfind("item +").unwrap();
        let line_index = LineIndex::new(src);
        let (line, column) = line_index.line_col(use_offset, src);
        let position = Position::new(line as u32, column as u32);

        let (_, detail) = server.hover_at_uri(&uri, position).expect("closure hover");
        assert_eq!(detail, "closure parameter item: i64");
        let (_, definition) = server
            .definition_at_uri(&uri, position)
            .expect("closure goto definition");
        assert!(definition.start.character < position.character);
        assert_eq!(
            server
                .references_at_uri(&uri, position, true)
                .expect("closure references")
                .len(),
            2
        );
        let edit = server
            .rename_at_uri(&uri, position, "element")
            .expect("closure rename");
        assert_eq!(edit.changes.unwrap().values().next().unwrap().len(), 2);
    }

    #[test]
    fn completion_local_not_offered_before_declaration() {
        // A `let` binding is not in scope before its own statement completes.
        let src = "fn main() {  let x = 1; }";
        let off = src.find("{ ").unwrap() + 2; // right after the opening brace
        let items = completion_items_at(src, off);
        assert!(
            !items.iter().any(|i| i.label == "x"),
            "x must not be offered before its declaration"
        );
    }

    #[test]
    fn completion_vec_receiver_lists_builtin_methods_without_globals() {
        // `v.` is a member slot (Vec receiver): it lists Vec's built-in methods
        // and crucially does NOT fall back to keywords/globals.
        let src = "fn main() { let v = vec![1]; v. }";
        let items = completion_items_at(src, src.rfind('.').unwrap() + 1);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(!labels.contains(&"fn"), "member slot must not surface keywords");
        assert!(labels.contains(&"push"), "Vec methods listed: {labels:?}");
        assert!(labels.contains(&"len"));
    }

    #[test]
    fn member_to_item_maps_kinds() {
        let f = member_to_item(CompletionMember {
            name: "x".into(),
            kind: MemberKind::Field,
            detail: "x: i64".into(),
        });
        assert_eq!(f.kind, Some(CompletionItemKind::FIELD));
        assert_eq!(f.detail.as_deref(), Some("x: i64"));

        let m = member_to_item(CompletionMember {
            name: "go".into(),
            kind: MemberKind::Method,
            detail: "fn go(&self) -> i64".into(),
        });
        assert_eq!(m.kind, Some(CompletionItemKind::METHOD));
    }

    // --- method `()` snippets ------------------------------------------------

    #[test]
    fn method_no_args_snippet_places_cursor_after_parens() {
        let m = member_to_item(CompletionMember {
            name: "go".into(),
            kind: MemberKind::Method,
            detail: "fn go(&self) -> i64".into(),
        });
        assert_eq!(m.insert_text.as_deref(), Some("go()$0"));
        assert_eq!(m.insert_text_format, Some(InsertTextFormat::SNIPPET));
    }

    #[test]
    fn method_with_args_snippet_places_cursor_inside_parens() {
        let m = member_to_item(CompletionMember {
            name: "add".into(),
            kind: MemberKind::Method,
            detail: "fn add(&self, n: i64) -> i64".into(),
        });
        assert_eq!(m.insert_text.as_deref(), Some("add($0)"));
        assert_eq!(m.insert_text_format, Some(InsertTextFormat::SNIPPET));
    }

    #[test]
    fn field_item_has_no_snippet() {
        let f = member_to_item(CompletionMember {
            name: "x".into(),
            kind: MemberKind::Field,
            detail: "x: i64".into(),
        });
        assert!(f.insert_text.is_none());
        assert!(f.insert_text_format.is_none());
    }

    // --- `Enum::` variant completion -----------------------------------------

    #[test]
    fn enum_path_completion_returns_variants_only() {
        let src = "enum Color { Red, Green, Blue }\nfn main() { let c = Color:: }";
        let items = completion_items_at(src, src.rfind("::").unwrap() + 2);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Red") && labels.contains(&"Green") && labels.contains(&"Blue"));
        assert!(!labels.contains(&"fn"), "path context must exclude keywords");
        assert_eq!(
            items.iter().find(|i| i.label == "Red").unwrap().kind,
            Some(CompletionItemKind::ENUM_MEMBER)
        );
    }

    // --- completion-item docs ------------------------------------------------

    #[test]
    fn completion_surfaces_doc_comment() {
        let src = "// greets the whole world\nfn hello() {}";
        let items = completions_for(src);
        let hello = items.iter().find(|i| i.label == "hello").unwrap();
        match hello.documentation.as_ref().expect("hello has docs") {
            Documentation::MarkupContent(m) => assert!(m.value.contains("greets the whole world")),
            other => panic!("expected markdown docs, got {other:?}"),
        }
    }

    #[test]
    fn undocumented_symbol_has_no_documentation() {
        let items = completions_for("fn plain() {}");
        let plain = items.iter().find(|i| i.label == "plain").unwrap();
        assert!(plain.documentation.is_none());
    }

    // --- builtin_hover_detail tests -------------------------------------------

    #[test]
    fn builtin_hover_some() {
        let detail = builtin_hover_detail("Some").expect("Some should have hover");
        assert!(detail.contains("Option<T>"));
    }

    #[test]
    fn builtin_hover_none() {
        let detail = builtin_hover_detail("None").expect("None should have hover");
        assert!(detail.contains("Option<T>"));
    }

    #[test]
    fn builtin_hover_ok() {
        let detail = builtin_hover_detail("Ok").expect("Ok should have hover");
        assert!(detail.contains("Result<T, E>"));
    }

    #[test]
    fn builtin_hover_err() {
        let detail = builtin_hover_detail("Err").expect("Err should have hover");
        assert!(detail.contains("Result<T, E>"));
    }

    #[test]
    fn builtin_hover_bool_literals() {
        for lit in &["true", "false"] {
            let detail = builtin_hover_detail(lit).expect("bool literal should have hover");
            assert!(detail.contains("bool"));
        }
    }

    #[test]
    fn builtin_hover_unknown_is_none() {
        assert!(builtin_hover_detail("foobar").is_none());
        assert!(builtin_hover_detail("fn").is_none());
        assert!(builtin_hover_detail("").is_none());
    }

    #[test]
    fn builtin_hover_macros() {
        for m in &[
            "vec", "println", "print", "format", "panic", "assert", "assert_eq",
            "assert_ne", "unreachable", "unimplemented", "todo", "dbg",
            "include_str", "include_bytes",
        ] {
            let detail = builtin_hover_detail(m).expect("macro should have hover");
            assert!(!detail.is_empty(), "hover for `{m}` should not be empty");
        }
    }
}
