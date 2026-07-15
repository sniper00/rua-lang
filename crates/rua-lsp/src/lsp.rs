//! `rua-lsp` — the Rua Language Server.
//!
//! Communicates via stdio JSON-RPC (LSP). All semantic queries go through a
//! single long-lived [`AnalysisHost`]; there is no legacy workspace or compiler
//! bridge in the production path.

#[path = "lsp/conversion.rs"]
mod conversion;
#[path = "lsp/filesystem.rs"]
mod filesystem;
#[path = "lsp/protocol.rs"]
mod protocol;
#[path = "lsp/requests.rs"]
mod requests;
#[path = "lsp/state.rs"]
mod state;
mod worker;

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    Cancel, DidChangeConfiguration, DidChangeTextDocument, DidChangeWatchedFiles,
    DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument, Exit, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{
    CallHierarchyIncomingCalls, CallHierarchyOutgoingCalls, CallHierarchyPrepare,
    CodeActionRequest, CodeLensRequest, Completion, DocumentHighlightRequest, DocumentLinkRequest,
    DocumentSymbolRequest, ExecuteCommand, FoldingRangeRequest, Formatting, GotoDefinition,
    GotoImplementation, HoverRequest, InlayHintRequest, OnTypeFormatting, PrepareRenameRequest,
    RangeFormatting, References, Rename, Request as _, ResolveCompletionItem,
    SelectionRangeRequest, SemanticTokensFullRequest, SemanticTokensRangeRequest, Shutdown,
    SignatureHelpRequest, TypeHierarchyPrepare, TypeHierarchySubtypes, TypeHierarchySupertypes,
    WorkspaceSymbolRequest,
};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionParams, CodeActionResponse, CodeLens, CodeLensParams,
    CompletionItem, CompletionItemKind, CompletionList, CompletionOptions, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeWatchedFilesParams, DocumentFormattingParams,
    DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams, DocumentLink,
    DocumentLinkParams, DocumentOnTypeFormattingParams, DocumentSymbol, DocumentSymbolResponse,
    Documentation, FileSystemWatcher, FoldingRange, FoldingRangeKind, FoldingRangeParams,
    GotoDefinitionResponse, Hover, HoverContents, HoverProviderCapability, InitializeParams,
    InlayHint, InlayHintKind, InlayHintLabel, InlayHintLabelPart, InlayHintParams,
    InsertTextFormat, Location, MarkupContent, MarkupKind, OneOf, ParameterInformation,
    ParameterLabel, Position, PrepareRenameResponse, PublishDiagnosticsParams, Range, Registration,
    RegistrationParams, RenameOptions, SelectionRange, SelectionRangeParams,
    SemanticToken as LspSemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensResult,
    SemanticTokensServerCapabilities, ServerCapabilities, SignatureHelp, SignatureHelpOptions,
    SignatureInformation, SymbolKind as LspSymbolKind, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextEdit, Unregistration, UnregistrationParams, Uri, WatchKind,
    WorkspaceEdit, WorkspaceSymbol, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};

use rua_analysis::{
    AnalysisHost, BuiltinDefinitionTarget, Change, CompletionInsert, CompletionKind, DefId,
    DefKind, FileId, FileKind, HoverResult, MacroDelimiter, NavigationTarget, ProjectData,
    ProjectFile, ProjectId, ProjectPosition, ProjectRoot, QueryContext, ReferenceResult,
    SemanticTokenKind, SourceChange, SourceRootId, SourceRootKind, TextRange,
    WorkspaceSymbol as AnalysisWorkspaceSymbol,
};
use rua_syntax::LineIndex;

use crate::conversion::{
    LineIndexCache, find_import_insertion_point, normalize_physical_path, path_to_uri,
    range_from_bytes, uri_to_path,
};
use crate::filesystem::{LibraryConfig, LibraryScanRequest, WorkspaceScan, scan_workspace_roots};
#[cfg(test)]
use crate::filesystem::{scan_library_root, scan_workspace_files};
use crate::state::*;
use crate::worker::WorkerPool;

// ---------------------------------------------------------------------------
// request-handler macros — factor out the 15-line extract→error→call→respond
// template repeated in ~28 handlers, saving ~400 lines of boilerplate.
// ---------------------------------------------------------------------------

/// Dispatch a position-based request (hover, goto-def, references, etc.).
/// The callback receives the resolved `ProjectPosition` and an `Analysis`
/// snapshot; it must return `Option<T>` for some serializable `T`.
macro_rules! handle_position_request {
    ($self:ident, $req:ident, $Params:ty, $method:expr,
     |$pp:ident, $analysis:ident| $body:expr) => {{
        let id = $req.id.clone();
        let (id, params) = match $req.extract::<$Params>($method) {
            Ok(v) => v,
            Err(e) => {
                let resp = Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    format!("invalid params: {e:?}"),
                );
                let _ = $self.connection.sender.send(Message::Response(resp));
                return;
            }
        };
        let $analysis = $self.host.analysis();
        let result = $self
            .project_position(
                &params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            )
            .and_then(|$pp| $body);
        let resp = Response::new_ok(id, result);
        let _ = $self.connection.sender.send(Message::Response(resp));
    }};
}

/// Dispatch a document-based request (semantic tokens, folding, symbols,
/// etc.).  The callback receives a `FileId` and an `Analysis` snapshot.
/// `$empty` is the empty/default response returned when the file is unknown.
macro_rules! handle_doc_request {
    ($self:ident, $req:ident, $Params:ty, $method:expr, $empty:expr,
     |$file_id:ident, $analysis:ident| $body:expr) => {{
        let id = $req.id.clone();
        let (id, params) = match $req.extract::<$Params>($method) {
            Ok(v) => v,
            Err(e) => {
                let resp = Response::new_err(
                    id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    format!("invalid params: {e:?}"),
                );
                let _ = $self.connection.sender.send(Message::Response(resp));
                return;
            }
        };
        let Some($file_id) = $self.file_id_for_uri(&params.text_document.uri) else {
            let resp = Response::new_ok(id, $empty);
            let _ = $self.connection.sender.send(Message::Response(resp));
            return;
        };
        let $analysis = $self.host.analysis();
        let result = $body;
        let resp = Response::new_ok(id, result);
        let _ = $self.connection.sender.send(Message::Response(resp));
    }};
}

/// Shorthand for parsing a notification body.  Prints a log line and returns
/// from the enclosing function on parse failure.
macro_rules! extract_notification {
    ($not:expr, $T:ty, $label:literal, |$params:ident| $body:expr) => {{
        match serde_json::from_value::<$T>($not.params) {
            Ok($params) => $body,
            Err(e) => {
                eprintln!("rua-lsp: bad {} params: {e}", $label);
                return;
            }
        }
    }};
}

#[path = "lsp/notifications.rs"]
mod notifications;

impl Server {
    // -- file identity -------------------------------------------------------

    fn doc_key(uri: &Uri) -> PathBuf {
        uri_to_path(uri)
            .map(|path| normalize_physical_path(&path))
            .unwrap_or_else(|| PathBuf::from(uri.as_str()))
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

    fn consider_project_root(&mut self, project_id: ProjectId, file_id: FileId, path: &Path) {
        let Some(project) = self.projects.get_mut(&project_id) else {
            return;
        };
        if path.file_name().and_then(|name| name.to_str()) == Some("main.rua") {
            project.root_file = file_id;
        }
    }

    fn set_project_changes(&self, change: &mut Change) {
        for (project_id, project) in &self.projects {
            change.set_project(
                *project_id,
                ProjectData::new(
                    project.root_file,
                    project.workspace_roots.clone(),
                    self.project_dependency_roots
                        .get(project_id)
                        .cloned()
                        .unwrap_or_default(),
                ),
            );
        }
    }

    fn rebuild_project_dependency_roots(&mut self) {
        self.project_dependency_roots.clear();
        let Some(root_id) = self.library_source_root else {
            return;
        };
        for project_id in self.projects.keys().copied() {
            let mut bases = self.library_bases.clone();
            if let Some(project_bases) = self.library_project_bases.get(&project_id.index()) {
                bases.extend(project_bases.iter().cloned());
            }
            bases.sort();
            bases.dedup();
            self.project_dependency_roots.insert(
                project_id,
                bases
                    .into_iter()
                    .map(|base| ProjectRoot::new(root_id, base))
                    .collect(),
            );
        }
    }

    fn project_id_for_file(&self, file_id: FileId) -> Option<ProjectId> {
        self.file_projects.get(&file_id).copied()
    }

    fn project_file(&self, file_id: FileId) -> Option<ProjectFile> {
        Some(ProjectFile::new(
            self.project_id_for_file(file_id)?,
            file_id,
        ))
    }

    fn project_id_for_path(&self, path: &Path) -> Option<ProjectId> {
        self.projects
            .iter()
            .filter_map(|(project_id, project)| {
                let best = project
                    .workspace_roots
                    .iter()
                    .filter(|root| path.starts_with(root.logical_base().as_path()))
                    .map(|root| root.logical_base().as_path().components().count())
                    .max()?;
                Some((best, *project_id))
            })
            .max_by_key(|(depth, project_id)| (*depth, *project_id))
            .map(|(_, project_id)| project_id)
    }

    fn apply_analysis_change(&mut self, change: Change) {
        self.input_generation = self.input_generation.wrapping_add(1);
        self.host.apply_change(change);
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
        let edits = if let Some((_, text)) = self
            .file_id_for_uri(&params.text_document.uri)
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
        let result: Option<Vec<TextEdit>> = if edits.is_empty() { None } else { Some(edits) };
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
        let (id, params): (_, SelectionRangeParams) =
            match req.extract::<SelectionRangeParams>(SelectionRangeRequest::METHOD) {
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
        let Some((source, line_index)) = self.source_line_index(file_id) else {
            let resp = Response::new_ok(id, Option::<Vec<SelectionRange>>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };

        let mut results = Vec::new();
        for pos in &params.positions {
            let offset = line_index.offset(pos.line as usize, pos.character as usize, &source);
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
        let Some((source, line_index)) = self.source_line_index(file_id) else {
            let resp = Response::new_ok(id, Option::<Vec<CodeLens>>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
        let Some(project_id) = self.project_id_for_file(file_id) else {
            let resp = Response::new_ok(id, Vec::<CodeLens>::new());
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
        let Some(def_map) = analysis.def_map_for_project(project_id) else {
            let resp = Response::new_ok(id, Vec::<CodeLens>::new());
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
        let Some(reference_index) = analysis.reference_index_for_project(project_id) else {
            let resp = Response::new_ok(id, Vec::<CodeLens>::new());
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
        let Some(member_index) = analysis.member_index_for_project(project_id) else {
            let resp = Response::new_ok(id, Vec::<CodeLens>::new());
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };

        let mut lenses = Vec::new();

        for definition in def_map.definitions() {
            if definition.file_id() != file_id {
                continue;
            }
            let kind = definition.kind();
            let title = match kind {
                rua_analysis::DefKind::Function | rua_analysis::DefKind::Method => {
                    let ref_count = reference_index.occurrences(definition.id()).len();
                    format!("{ref_count} reference(s)")
                }
                rua_analysis::DefKind::Struct => {
                    let impl_count = member_index
                        .implementations()
                        .filter(|implementation| {
                            matches!(
                                implementation.target_ty(),
                                rua_analysis::Ty::Named(named)
                                    if named.definition() == definition.id()
                            )
                        })
                        .count();
                    if impl_count == 0 {
                        "struct".to_string()
                    } else {
                        format!("{impl_count} impl(s)")
                    }
                }
                rua_analysis::DefKind::Trait => {
                    let impl_count = member_index
                        .implementations()
                        .filter(|implementation| {
                            implementation.trait_definition() == Some(definition.id())
                        })
                        .count();
                    if impl_count == 0 {
                        "trait".to_string()
                    } else {
                        format!("{impl_count} impl(s)")
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
        handle_position_request!(
            self,
            req,
            lsp_types::HoverParams,
            HoverRequest::METHOD,
            |pp, analysis| { analysis.hover(pp).map(|hover| to_lsp_hover(&hover)) }
        );
    }

    // -- goto definition -----------------------------------------------------

    fn handle_definition(&mut self, req: Request) {
        handle_position_request!(
            self,
            req,
            lsp_types::GotoDefinitionParams,
            GotoDefinition::METHOD,
            |pp, analysis| {
                analysis
                    .goto_definition(pp)
                    .and_then(|target| self.nav_to_location(&target))
                    .or_else(|| {
                        analysis
                            .goto_builtin_definition(pp)
                            .and_then(|target| self.std_target_to_location(&target))
                    })
            }
        );
    }

    // -- goto implementation -------------------------------------------------

    fn handle_goto_implementation(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params) =
            match req.extract::<lsp_types::GotoDefinitionParams>(GotoImplementation::METHOD) {
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
                        let range = self.range_for_file(file_range.file_id, file_range.range)?;
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
        let symbols = self
            .project_file(file_id)
            .map_or_else(Vec::new, |file| analysis.document_symbols_in_project(file));
        let Some((source, line_index)) = self.source_line_index(file_id) else {
            let resp = Response::new_ok(id, Option::<DocumentSymbolResponse>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
        let nested = build_document_symbol_tree(&symbols, &line_index, &source);
        let result = DocumentSymbolResponse::Nested(nested);

        let resp = Response::new_ok(id, Some(result));
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- completion ----------------------------------------------------------

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

        if self.file_id_for_uri(uri).is_none() {
            let resp = Response::new_ok(id, CompletionResponse::Array(Vec::new()));
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };

        let analysis = self.host.analysis();

        let items = self
            .project_position(uri, pos)
            .and_then(|pp| {
                let (source, line_index) = self.source_line_index(pp.position.file_id)?;
                let native_items = analysis.completions(pp);
                let file_id = pp.position.file_id;
                Some(
                    native_items
                        .into_iter()
                        .map(|item| completion_to_lsp(&item, &line_index, &source, file_id))
                        .collect::<Vec<_>>(),
                )
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
        handle_position_request!(
            self,
            req,
            lsp_types::SignatureHelpParams,
            SignatureHelpRequest::METHOD,
            |pp, analysis| {
                analysis.signature_help(pp).map(|info| {
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
                })
            }
        );
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
        let Some((source, line_index)) = self.source_line_index(file_id) else {
            let resp = Response::new_ok(id, Option::<Vec<InlayHint>>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };

        let Some(project_file) = self.project_file(file_id) else {
            let resp = Response::new_ok(id, Vec::<InlayHint>::new());
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
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

        let mut hints = Vec::new();
        for hint in analysis.inlay_hints(project_file) {
            let offset = hint.position().offset as usize;
            if offset < range_start || offset > range_end || offset > source.len() {
                continue;
            }
            let (line, col) = line_index.line_col(offset, &source);
            let value = format!(": {}", hint.ty());
            let location = hint.target().and_then(|target| {
                Some(Location {
                    uri: self.uri_for_file(target.file_id)?,
                    range: self.range_for_file(target.file_id, target.range)?,
                })
            });
            let label = match location {
                Some(location) => InlayHintLabel::LabelParts(vec![InlayHintLabelPart {
                    value,
                    tooltip: None,
                    location: Some(location),
                    command: None,
                }]),
                None => InlayHintLabel::String(value),
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

        let Some((source, line_index)) = self.source_line_index(file_id) else {
            let resp = Response::new_ok(id, Option::<Vec<TextEdit>>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };

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
            let (line, col) = (edit_pos.line, 0u32);
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

        let result = if edits.is_empty() { None } else { Some(edits) };
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
        let Some((source, line_index)) = self.source_line_index(file_id) else {
            let resp = Response::new_ok(id, Option::<CodeActionResponse>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };

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
        let project_id = self.project_id_for_file(file_id);

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
                    "E0212" => {
                        let diag_range = &diag.range;
                        let Some(project_id) = project_id else {
                            continue;
                        };
                        let diagnostic_offset = line_index.offset(
                            diag_range.start.line as usize,
                            diag_range.start.character as usize,
                            &source,
                        );
                        let Some(target) = analysis.prepare_rename(ProjectPosition::at(
                            project_id,
                            file_id,
                            diagnostic_offset as u32,
                        )) else {
                            continue;
                        };
                        let declaration = target.range();
                        if declaration.file_id != file_id {
                            continue;
                        }
                        let insert_pos = declaration.range.start() as usize;
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
        let Some(project_id) = project_id else {
            let resp = Response::new_ok(id, actions);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
        let Some(def_map) = analysis.def_map_for_project(project_id) else {
            let resp = Response::new_ok(id, actions);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
        let Some(member_index) = analysis.member_index_for_project(project_id) else {
            let resp = Response::new_ok(id, actions);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
        let Some(semantics) = analysis.semantics_for_project(project_id) else {
            let resp = Response::new_ok(id, actions);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
        for definition in def_map.definitions() {
            if !definition.kind().is_body_owner() {
                continue;
            }
            let Some(body) = analysis.body(definition.id()) else {
                continue;
            };
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
                let Some(scrutinee_ty) = scrutinee_ty else {
                    continue;
                };

                // Check if enum
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

                let all_variants = member_index.associated_candidates(&enum_template);
                let line_start = source[..m_start]
                    .rfind('\n')
                    .map_or(0, |newline| newline + 1);
                let base_indent = source[line_start..m_start]
                    .chars()
                    .take_while(|character| matches!(character, ' ' | '\t'))
                    .collect::<String>();
                let arm_indent = format!("{base_indent}    ");
                let existing_variants: HashSet<DefId> = arms
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
                        let range = source_map.name_ref_range(*last_seg)?;
                        semantics
                            .find_def_at(rua_analysis::FilePosition::new(
                                range.file_id,
                                range.range.start(),
                            ))
                            .map(|definition| definition.id())
                    })
                    .collect();

                let mut new_arms = String::new();
                let mut missing_count = 0;
                for variant in &all_variants {
                    let rua_analysis::MemberTarget::Definition(variant_id) = variant.target()
                    else {
                        continue;
                    };
                    let name = variant.name();
                    if existing_variants.contains(&variant_id) {
                        continue;
                    }
                    let Some(variant_definition) = def_map.definition(variant_id) else {
                        continue;
                    };
                    let rua_analysis::ItemSignature::Variant(signature) =
                        variant_definition.signature()
                    else {
                        continue;
                    };
                    let pattern = match signature.kind() {
                        rua_analysis::VariantKind::Unit => name.to_string(),
                        rua_analysis::VariantKind::Tuple => format!(
                            "{name}({})",
                            vec!["_"; signature.tuple_types().len()].join(", ")
                        ),
                        rua_analysis::VariantKind::Struct => {
                            format!("{name} {{ .. }}")
                        }
                    };
                    if !new_arms.is_empty() {
                        new_arms.push('\n');
                    }
                    new_arms.push_str(&format!("{arm_indent}{pattern} => todo!(),"));
                    missing_count += 1;
                }
                if new_arms.is_empty() {
                    continue;
                }

                let Some(scrutinee_range) = source_map.expr_range(*scrutinee) else {
                    continue;
                };
                let search_start = scrutinee_range.range.end() as usize;
                let Some(relative_lbrace) = source[search_start..m_end].find('{') else {
                    continue;
                };
                let lbrace = search_start + relative_lbrace;
                let insert_pos = lbrace + 1;
                let suffix = if source.as_bytes().get(insert_pos) == Some(&b'\n') {
                    String::new()
                } else {
                    format!("\n{base_indent}")
                };

                let (line, col) = line_index.line_col(insert_pos, &source);
                let edit = TextEdit {
                    range: Range {
                        start: Position::new(line as u32, col as u32),
                        end: Position::new(line as u32, col as u32),
                    },
                    new_text: format!("\n{new_arms}{suffix}"),
                };

                let mut changes = std::collections::HashMap::new();
                changes.insert(uri.clone(), vec![edit]);
                let workspace_edit = WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                };

                actions.push(CodeAction {
                    title: format!("Fill match arms ({missing_count} missing)"),
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

        // "Remove trailing comma" — if cursor is on a comma before )/]/}.
        let comma_offset = line_index.offset(
            params.range.start.line as usize,
            params.range.start.character as usize,
            &source,
        );
        if comma_offset < source.len() && source.as_bytes().get(comma_offset) == Some(&b',') {
            let after: String = source[comma_offset + 1..]
                .chars()
                .take_while(|c| c.is_whitespace())
                .collect();
            let next = source[comma_offset + 1 + after.len()..].chars().next();
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

        let resp = Response::new_ok(id, actions);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- call hierarchy ------------------------------------------------------

    fn handle_call_hierarchy_prepare(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, lsp_types::CallHierarchyPrepareParams) =
            match req.extract::<lsp_types::CallHierarchyPrepareParams>(CallHierarchyPrepare::METHOD)
            {
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
            analysis.call_hierarchy_prepare(pp).and_then(|item| {
                self.call_hierarchy_item_to_lsp(&item)
                    .map(|item| vec![item])
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
                let Some((project_id, target)) = hierarchy_identity(params.item.data.as_ref())
                else {
                    return Vec::new();
                };
                if self.project_id_for_file(file_id) != Some(project_id) {
                    return Vec::new();
                }
                let chi = rua_analysis::CallHierarchyItem {
                    project_id,
                    target,
                    name: params.item.name.clone(),
                    kind: rua_analysis::DefKind::Function,
                    file_id,
                    range: self
                        .text_range_for_file(file_id, params.item.selection_range)
                        .unwrap_or_else(|| TextRange::new(0, 0)),
                    call_sites: Vec::new(),
                };
                analysis
                    .call_hierarchy_incoming(&chi)
                    .into_iter()
                    .filter_map(|item| {
                        let from = self.call_hierarchy_item_to_lsp(&item)?;
                        let from_ranges = item
                            .call_sites
                            .iter()
                            .filter_map(|range| self.range_for_file(range.file_id, range.range))
                            .collect::<Vec<_>>();
                        (!from_ranges.is_empty())
                            .then_some(lsp_types::CallHierarchyIncomingCall { from, from_ranges })
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
                let Some((project_id, target)) = hierarchy_identity(params.item.data.as_ref())
                else {
                    return Vec::new();
                };
                if self.project_id_for_file(file_id) != Some(project_id) {
                    return Vec::new();
                }
                let chi = rua_analysis::CallHierarchyItem {
                    project_id,
                    target,
                    name: params.item.name.clone(),
                    kind: rua_analysis::DefKind::Function,
                    file_id,
                    range: self
                        .text_range_for_file(file_id, params.item.selection_range)
                        .unwrap_or_else(|| TextRange::new(0, 0)),
                    call_sites: Vec::new(),
                };
                analysis
                    .call_hierarchy_outgoing(&chi)
                    .into_iter()
                    .filter_map(|item| {
                        let to = self.call_hierarchy_item_to_lsp(&item)?;
                        let from_ranges = item
                            .call_sites
                            .iter()
                            .filter_map(|range| self.range_for_file(range.file_id, range.range))
                            .collect::<Vec<_>>();
                        (!from_ranges.is_empty())
                            .then_some(lsp_types::CallHierarchyOutgoingCall { to, from_ranges })
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
            match req.extract::<lsp_types::TypeHierarchyPrepareParams>(TypeHierarchyPrepare::METHOD)
            {
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
                .map(|item| self.type_hierarchy_item_to_lsp(&item, to_lsp_symbol_kind(item.kind)))
                .and_then(|item| item.map(|item| vec![item]))
        });
        let resp = Response::new_ok(id, result);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    fn handle_type_hierarchy_subtypes(&mut self, req: Request) {
        let id = req.id.clone();
        let (id, params): (_, lsp_types::TypeHierarchySubtypesParams) =
            match req
                .extract::<lsp_types::TypeHierarchySubtypesParams>(TypeHierarchySubtypes::METHOD)
            {
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
                let Some((project_id, target)) = hierarchy_identity(params.item.data.as_ref())
                else {
                    return Vec::new();
                };
                if self.project_id_for_file(file_id) != Some(project_id) {
                    return Vec::new();
                }
                let thi = rua_analysis::TypeHierarchyItem {
                    project_id,
                    target,
                    name: params.item.name.clone(),
                    kind: rua_analysis::DefKind::Trait,
                    file_id,
                    range: self
                        .text_range_for_file(file_id, params.item.selection_range)
                        .unwrap_or_else(|| TextRange::new(0, 0)),
                };
                analysis
                    .type_hierarchy_subtypes(&thi)
                    .into_iter()
                    .filter_map(|item| {
                        self.type_hierarchy_item_to_lsp(&item, lsp_types::SymbolKind::STRUCT)
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
                let Some((project_id, target)) = hierarchy_identity(params.item.data.as_ref())
                else {
                    return Vec::new();
                };
                if self.project_id_for_file(file_id) != Some(project_id) {
                    return Vec::new();
                }
                let thi = rua_analysis::TypeHierarchyItem {
                    project_id,
                    target,
                    name: params.item.name.clone(),
                    kind: rua_analysis::DefKind::Struct,
                    file_id,
                    range: self
                        .text_range_for_file(file_id, params.item.selection_range)
                        .unwrap_or_else(|| TextRange::new(0, 0)),
                };
                analysis
                    .type_hierarchy_supertypes(&thi)
                    .into_iter()
                    .filter_map(|item| {
                        self.type_hierarchy_item_to_lsp(&item, lsp_types::SymbolKind::INTERFACE)
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
                let Some((source, line_index)) = self.source_line_index(*file_id) else {
                    continue;
                };
                let mut file_edits = Vec::new();
                let mut search_start = 0usize;
                while let Some(pos) = source[search_start..].find(pattern) {
                    let abs_pos = search_start + pos;
                    if !pattern.is_empty() {
                        let (sl, sc) = line_index.line_col(abs_pos, &source);
                        let (el, ec) = line_index.line_col(abs_pos + pattern.len(), &source);
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

        let resp: Response = Response::new_ok(id, Option::<serde_json::Value>::None);
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
        let project_ids = self.projects.keys().copied().collect::<Vec<_>>();
        let task_id = self.next_task_id;
        self.next_task_id = self.next_task_id.wrapping_add(1);
        let cancellation = CancellationToken::new();
        self.pending_queries.insert(
            task_id,
            PendingQuery {
                request_id: id.clone(),
                input_generation: self.input_generation,
                cancellation: cancellation.clone(),
            },
        );
        let sender = self.background_sender.clone();
        if self
            .worker_pool
            .try_execute(move || {
                let mut symbols = Vec::new();
                for project_id in project_ids {
                    if cancellation.is_cancelled() {
                        let _ = sender.send(BackgroundResult::WorkspaceSymbols {
                            task_id,
                            result: None,
                        });
                        return;
                    }
                    symbols.extend(
                        analysis
                            .workspace_symbols_in_project(QueryContext::new(project_id), &query),
                    );
                }
                let result = (!cancellation.is_cancelled()).then_some(symbols);
                let _ = sender.send(BackgroundResult::WorkspaceSymbols { task_id, result });
            })
            .is_err()
        {
            self.pending_queries.remove(&task_id);
            self.send_query_error(
                id,
                lsp_server::ErrorCode::ServerCancelled,
                "analysis worker queue is full",
            );
        }
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

        let Some((source, line_index)) = self.source_line_index(file_id) else {
            let resp = Response::new_ok(id, Option::<Vec<FoldingRange>>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
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

        let Some((source, line_index)) = self.source_line_index(file_id) else {
            let resp = Response::new_ok(id, Option::<Vec<DocumentLink>>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
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

        // Resolve documentation through the exact semantic declaration target.
        if let Some(raw) = &item.data
            && let Some(target) = parse_resolve_data(raw)
        {
            let analysis = self.host.analysis();
            if let Some(project_id) = self.project_id_for_file(target.file_id)
                && let Some(def_map) = analysis.def_map_for_project(project_id)
                && let Some(definition) = def_map.definitions().find(|definition| {
                    definition.file_id() == target.file_id
                        && definition.name_range() == target.range
                })
                && let Some(documentation) = definition.documentation()
            {
                item.documentation = Some(Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: documentation.to_string(),
                }));
            }
        }

        let resp = Response::new_ok(id, item);
        let _ = self.connection.sender.send(Message::Response(resp));
    }

    // -- references ----------------------------------------------------------

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

        let Some(project_position) = self.project_position(uri, pos) else {
            let response = Response::new_ok(id, Option::<Vec<Location>>::None);
            let _ = self.connection.sender.send(Message::Response(response));
            return;
        };
        let analysis = self.host.analysis();
        let task_id = self.next_task_id;
        self.next_task_id = self.next_task_id.wrapping_add(1);
        let cancellation = CancellationToken::new();
        self.pending_queries.insert(
            task_id,
            PendingQuery {
                request_id: id.clone(),
                input_generation: self.input_generation,
                cancellation: cancellation.clone(),
            },
        );
        let sender = self.background_sender.clone();
        if self
            .worker_pool
            .try_execute(move || {
                let result =
                    analysis.references_cancellable(project_position, include_decl, || {
                        cancellation.is_cancelled()
                    });
                let _ = sender.send(BackgroundResult::References { task_id, result });
            })
            .is_err()
        {
            self.pending_queries.remove(&task_id);
            self.send_query_error(
                id,
                lsp_server::ErrorCode::ServerCancelled,
                "analysis worker queue is full",
            );
        }
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

        let result = self.project_position(uri, pos).and_then(|pp| {
            let analysis = self.host.analysis();
            match analysis.rename(pp, &params.new_name) {
                Ok(change) => source_change_to_workspace_edit(
                    &change,
                    |file_id| self.source_line_index(file_id),
                    |file_id| self.file_to_uri.get(&file_id).cloned(),
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
        let (id, params) = match req
            .extract::<lsp_types::TextDocumentPositionParams>(PrepareRenameRequest::METHOD)
        {
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
        handle_doc_request!(
            self,
            req,
            lsp_types::SemanticTokensParams,
            SemanticTokensFullRequest::METHOD,
            Option::<SemanticTokensResult>::None,
            |file_id, analysis| {
                self.source_line_index(file_id).map(|(source, line_index)| {
                    let tokens = self
                        .project_file(file_id)
                        .map_or_else(Vec::new, |file| analysis.semantic_tokens_in_project(file));
                    let data = encode_semantic_tokens(&tokens, &line_index, &source);
                    SemanticTokensResult::Tokens(SemanticTokens {
                        result_id: None,
                        data,
                    })
                })
            }
        );
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
        let Some((source, line_index)) = self.source_line_index(file_id) else {
            let resp = Response::new_ok(id, Option::<SemanticTokensResult>::None);
            let _ = self.connection.sender.send(Message::Response(resp));
            return;
        };
        let tokens = self
            .project_file(file_id)
            .map_or_else(Vec::new, |file| analysis.semantic_tokens_in_project(file));
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

    fn open_document(&mut self, uri: Uri, version: i32, text: String) {
        let file_id = self.ensure_file_id(&uri);
        let path = Self::doc_key(&uri);
        let project_id = self
            .project_id_for_file(file_id)
            .or_else(|| self.project_id_for_path(&path))
            .unwrap_or(ProjectId::new(0));
        self.projects.entry(project_id).or_insert_with(|| {
            let root_id = SourceRootId::new(0);
            WorkspaceProject {
                root_file: file_id,
                workspace_roots: vec![ProjectRoot::new(
                    root_id,
                    path.parent().unwrap_or(Path::new("")),
                )],
            }
        });
        self.file_projects.insert(file_id, project_id);
        self.consider_project_root(project_id, file_id, &path);
        let mut change = Change::new();
        if self.host.analysis().file_kind(file_id).is_some() {
            change.set_file_text(file_id, &*text);
        } else {
            let root_id = self.projects[&project_id].workspace_roots[0].source_root_id();
            change.set_source_root(root_id, SourceRootKind::Workspace);
            change.set_file_with_path(file_id, root_id, FileKind::Source, path, &*text);
        }
        self.set_project_changes(&mut change);
        self.apply_analysis_change(change);
        self.open_buffers.insert(file_id, (uri.clone(), text));
        self.open_versions.insert(file_id, version);
        self.publish_diagnostics(&uri);
    }

    fn change_document(&mut self, uri: Uri, version: i32, text: String) {
        let Some(file_id) = self.file_id_for_uri(&uri) else {
            return;
        };
        let Some(previous_version) = self.open_versions.get(&file_id).copied() else {
            return;
        };
        if version <= previous_version {
            return;
        }
        let mut change = Change::new();
        change.set_file_text(file_id, &*text);
        self.apply_analysis_change(change);
        self.open_buffers.insert(file_id, (uri.clone(), text));
        self.open_versions.insert(file_id, version);
        self.publish_diagnostics(&uri);
    }

    fn close_document(&mut self, uri: Uri) {
        let key = Self::doc_key(&uri);
        if let Some(file_id) = self.file_ids.get(&key).map(|(_, file_id)| *file_id)
            && self.open_buffers.remove(&file_id).is_some()
        {
            self.open_versions.remove(&file_id);
            let mut change = Change::new();
            if let Some(disk) = self.disk_files.get(&file_id) {
                change.set_file_with_path(
                    file_id,
                    disk.source_root,
                    disk.kind,
                    &disk.analysis_path,
                    &*disk.text,
                );
            } else {
                change.remove_file(file_id);
            }
            self.apply_analysis_change(change);
        }
        self.send_diagnostics(&uri, &[], None);
    }

    fn handle_did_save(&mut self, params: &lsp_types::DidSaveTextDocumentParams) {
        let uri = &params.text_document.uri;
        if let Some(file_id) = self.file_id_for_uri(uri)
            && let Some(disk) = self.disk_files.get_mut(&file_id)
            && let Ok(text) = std::fs::read_to_string(&disk.path)
        {
            disk.text = text;
        }
        self.publish_diagnostics(uri);
    }

    fn reload_configuration(&mut self, settings: &serde_json::Value) {
        let settings =
            match crate::filesystem::merge_project_configs(settings, &self.workspace_folders) {
                Ok(settings) => settings,
                Err(error) => {
                    eprintln!("rua-lsp: project configuration failed: {error}");
                    return;
                }
            };
        let Ok(request) = LibraryScanRequest::from_settings(&settings) else {
            return;
        };
        if let Some((_, cancellation)) = self.library_scan.take() {
            cancellation.cancel();
        }
        let generation = self.next_scan_generation;
        self.next_scan_generation = self.next_scan_generation.wrapping_add(1);
        let cancellation = CancellationToken::new();
        self.library_scan = Some((generation, cancellation.clone()));
        let sender = self.background_sender.clone();
        if self
            .worker_pool
            .try_execute(move || {
                let result = request.scan(&mut || cancellation.is_cancelled());
                let _ = sender.send(BackgroundResult::LibraryScan {
                    generation,
                    result: Box::new(result),
                });
            })
            .is_err()
        {
            self.library_scan = None;
        }
    }

    fn apply_library_config(&mut self, config: LibraryConfig) {
        let standard_library = config
            .standard_library
            .as_ref()
            .map(Ok)
            .unwrap_or_else(|| rua_resources::embedded_std().map_err(ToString::to_string));
        if let Ok(standard_library) = standard_library {
            let _ = self.host.set_standard_library(standard_library);
        }
        self.standard_library_root = config.std_root.clone();
        self.standard_library_navigation_root = config.std_root.clone().or_else(|| {
            rua_resources::materialized_embedded_std()
                .ok()
                .map(Path::to_path_buf)
        });
        let mut change = Change::new();

        if let Some(old_root) = self.library_source_root.take() {
            change.remove_source_root(old_root);
        }
        for file_id in self.library_file_ids.drain() {
            self.disk_files.remove(&file_id);
            if let Some((_, overlay)) = self.open_buffers.get(&file_id) {
                let root = SourceRootId::new(0);
                change.set_source_root(root, SourceRootKind::Workspace);
                change.set_file_with_path(
                    file_id,
                    root,
                    FileKind::Declaration,
                    Self::doc_key(
                        self.file_to_uri
                            .get(&file_id)
                            .expect("open overlay retains URI"),
                    ),
                    &**overlay,
                );
            }
        }

        self.library_bases = config.bases.clone();
        self.library_project_bases = config.project_bases.clone();
        if !config.roots.is_empty() || !config.mounts.is_empty() {
            let root_id = SourceRootId::new(self.next_root_id);
            self.next_root_id += 1;
            self.library_source_root = Some(root_id);
            change.set_source_root(root_id, SourceRootKind::Library);
            for file in &config.files {
                let file_id = self.ensure_file_id_for_path(&file.physical_path);
                self.library_file_ids.insert(file_id);
                self.disk_files.insert(
                    file_id,
                    DiskFile {
                        path: file.physical_path.clone(),
                        analysis_path: file.analysis_path.clone(),
                        text: file.text.clone(),
                        source_root: root_id,
                        kind: FileKind::Declaration,
                    },
                );
                if !self.open_buffers.contains_key(&file_id) {
                    change.set_file_with_path(
                        file_id,
                        root_id,
                        FileKind::Declaration,
                        &file.analysis_path,
                        &*file.text,
                    );
                }
            }
        }
        self.rebuild_project_dependency_roots();

        self.library_roots = config.roots;
        self.library_mounts = config.mounts;
        self.set_project_changes(&mut change);
        self.apply_analysis_change(change);
        self.register_watchers();
    }

    // -- file watchers --------------------------------------------------------

    /// Try to add a path to the watch list, skipping duplicates.
    fn try_add_watcher(
        glob: &str,
        watched_paths: &mut Vec<PathBuf>,
        watchers: &mut Vec<FileSystemWatcher>,
    ) {
        if !watched_paths.iter().any(|p| p.to_string_lossy() == glob) {
            watchers.push(FileSystemWatcher {
                glob_pattern: lsp_types::GlobPattern::String(glob.to_string()),
                kind: Some(WatchKind::all()),
            });
            watched_paths.push(PathBuf::from(glob));
        }
    }

    /// Register `workspace/didChangeWatchedFiles` for configured library roots.
    fn register_watchers(&mut self) {
        if let Some(registration_id) = self.watch_registration_id.take() {
            self.watch_registrations
                .entry(registration_id.clone())
                .or_default()
                .desired = false;
            let request_id = self.next_request_id;
            self.next_request_id += 1;
            let request_id: RequestId = request_id.into();
            let request = lsp_server::Request::new(
                request_id.clone(),
                "client/unregisterCapability".to_string(),
                UnregistrationParams {
                    unregisterations: vec![Unregistration {
                        id: registration_id.clone(),
                        method: DidChangeWatchedFiles::METHOD.to_string(),
                    }],
                },
            );
            self.pending_watch_requests
                .insert(request_id, (WatchOperation::Unregister, registration_id));
            let _ = self.connection.sender.send(Message::Request(request));
        }
        self.watched_paths.clear();
        let mut watchers: Vec<FileSystemWatcher> = Vec::new();

        for root in &self.library_roots {
            if let Ok(canonical) = std::fs::canonicalize(root) {
                let pattern = if canonical.is_dir() {
                    canonical.join("**/*.ruai")
                } else {
                    canonical
                };
                let glob = pattern.to_string_lossy().to_string();
                Self::try_add_watcher(&glob, &mut self.watched_paths, &mut watchers);
            }
        }
        for mount_path in self.library_mounts.values() {
            if let Ok(canonical) = std::fs::canonicalize(mount_path) {
                let glob = canonical.to_string_lossy().to_string();
                Self::try_add_watcher(&glob, &mut self.watched_paths, &mut watchers);
            }
        }
        if let Some(root) = &self.standard_library_root
            && let Ok(canonical) = std::fs::canonicalize(root)
        {
            let glob = canonical
                .join("**/*.{ruai,toml}")
                .to_string_lossy()
                .to_string();
            Self::try_add_watcher(&glob, &mut self.watched_paths, &mut watchers);
        }

        if watchers.is_empty() {
            return;
        }

        let request_id = self.next_request_id;
        self.next_request_id += 1;
        let registration_id = format!("rua-library-watcher-{request_id}");
        let registration = Registration {
            id: registration_id.clone(),
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
        let request_id: RequestId = request_id.into();
        let request = lsp_server::Request::new(
            request_id.clone(),
            "client/registerCapability".to_string(),
            params,
        );
        self.watch_registrations.insert(
            registration_id.clone(),
            WatchRegistrationState {
                desired: true,
                ..WatchRegistrationState::default()
            },
        );
        self.pending_watch_requests.insert(
            request_id,
            (WatchOperation::Register, registration_id.clone()),
        );
        let _ = self
            .connection
            .sender
            .send(lsp_server::Message::Request(request));
        self.watch_registration_id = Some(registration_id);
    }

    fn library_analysis_path(&self, path: &Path) -> PathBuf {
        for (name, mounted) in &self.library_mounts {
            let Ok(canonical) = std::fs::canonicalize(mounted) else {
                continue;
            };
            let base = canonical.parent().unwrap_or(Path::new(""));
            if canonical.is_file() && canonical == path {
                let extension = canonical
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .unwrap_or("ruai");
                return base.join(format!("{name}.{extension}"));
            }
            if canonical.is_dir()
                && let Ok(relative) = path.strip_prefix(&canonical)
            {
                return base.join(name).join(relative);
            }
        }
        path.to_path_buf()
    }

    fn handle_watched_file_change(&mut self, params: &DidChangeWatchedFilesParams) {
        let mut change = Change::new();
        let mut standard_library_changed = false;

        for event in &params.changes {
            let Some(path) = uri_to_path(&event.uri) else {
                continue;
            };
            let path = normalize_physical_path(&path);
            if self
                .standard_library_root
                .as_ref()
                .is_some_and(|root| path.starts_with(root))
            {
                standard_library_changed = true;
                continue;
            }
            let Some(root_id) = self.library_source_root else {
                continue;
            };
            let file_id = self.ensure_file_id_for_path(&path);
            match event.typ {
                lsp_types::FileChangeType::CREATED | lsp_types::FileChangeType::CHANGED => {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        let analysis_path = self.library_analysis_path(&path);
                        self.library_file_ids.insert(file_id);
                        self.disk_files.insert(
                            file_id,
                            DiskFile {
                                path: path.clone(),
                                analysis_path: analysis_path.clone(),
                                text: text.clone(),
                                source_root: root_id,
                                kind: FileKind::Declaration,
                            },
                        );
                        if !self.open_buffers.contains_key(&file_id) {
                            change.set_file_with_path(
                                file_id,
                                root_id,
                                FileKind::Declaration,
                                analysis_path,
                                &*text,
                            );
                        }
                    }
                }
                lsp_types::FileChangeType::DELETED => {
                    self.library_file_ids.remove(&file_id);
                    self.disk_files.remove(&file_id);
                    if !self.open_buffers.contains_key(&file_id) {
                        change.remove_file(file_id);
                    }
                }
                _ => {}
            }
        }

        if standard_library_changed
            && let Some(root) = &self.standard_library_root
            && let Ok(library) = rua_resources::load_std_dir(root)
        {
            let _ = self.host.set_standard_library(&library);
        }

        self.rebuild_project_dependency_roots();
        self.set_project_changes(&mut change);
        self.apply_analysis_change(change);

        // Republish diagnostics for any open files that may be affected.
        let open_uris: Vec<Uri> = self
            .open_buffers
            .values()
            .map(|(uri, _)| uri.clone())
            .collect();
        for uri in open_uris {
            self.publish_diagnostics(&uri);
        }
    }

    /// Register a file by filesystem path.  Delegates to [`ensure_file_id`] so
    /// that the canonical URI-to-path round-trip produces a key consistent with
    /// URI-registered files, avoiding duplicate FileIds.
    fn ensure_file_id_for_path(&mut self, path: &Path) -> FileId {
        let path = normalize_physical_path(path);
        if let Some((_, id)) = self.file_ids.get(&path) {
            return *id;
        }
        let uri = path_to_uri(&path).unwrap_or_else(|| {
            format!("file:///unknown/{}", self.next_file_id)
                .parse()
                .unwrap_or_else(|_| "file:///unknown.rua".parse().unwrap())
        });
        self.ensure_file_id(&uri)
    }

    // -- helpers -------------------------------------------------------------

    fn project_position(&self, uri: &Uri, pos: Position) -> Option<ProjectPosition> {
        let file_id = self.file_id_for_uri(uri)?;
        let (source, line_index) = self.source_line_index(file_id)?;
        let offset = line_index.offset(pos.line as usize, pos.character as usize, &source);
        Some(ProjectPosition::at(
            self.project_id_for_file(file_id)?,
            file_id,
            offset as u32,
        ))
    }

    fn range_for_file(&self, file_id: FileId, range: TextRange) -> Option<Range> {
        let (source, li) = self.source_line_index(file_id)?;
        let start = li.line_col(range.start() as usize, &source);
        let end = li.line_col(range.end() as usize, &source);
        Some(Range {
            start: Position::new(start.0 as u32, start.1 as u32),
            end: Position::new(end.0 as u32, end.1 as u32),
        })
    }

    fn text_range_for_file(&self, file_id: FileId, range: Range) -> Option<TextRange> {
        let (source, line_index) = self.source_line_index(file_id)?;
        let start = line_index.offset(
            range.start.line as usize,
            range.start.character as usize,
            &source,
        );
        let end = line_index.offset(
            range.end.line as usize,
            range.end.character as usize,
            &source,
        );
        (start <= end).then(|| TextRange::new(start as u32, end as u32))
    }

    fn nav_to_location(&self, target: &NavigationTarget) -> Option<GotoDefinitionResponse> {
        let file_range = target.target_range();
        let uri = self.uri_for_file(file_range.file_id)?;
        let range = self.range_for_file(file_range.file_id, file_range.range)?;
        Some(GotoDefinitionResponse::Scalar(Location { uri, range }))
    }

    fn std_target_to_location(
        &self,
        target: &BuiltinDefinitionTarget,
    ) -> Option<GotoDefinitionResponse> {
        let path = self
            .standard_library_navigation_root
            .as_ref()?
            .join(target.source_name());
        let source = std::fs::read_to_string(&path).ok()?;
        let line_index = LineIndex::new(&source);
        let start = line_index.line_col(target.range().start() as usize, &source);
        let end = line_index.line_col(target.range().end() as usize, &source);
        let uri = path_to_uri(&path)?;
        Some(GotoDefinitionResponse::Scalar(Location {
            uri,
            range: Range::new(
                Position::new(start.0 as u32, start.1 as u32),
                Position::new(end.0 as u32, end.1 as u32),
            ),
        }))
    }

    fn ref_to_location(&self, r: &rua_analysis::ReferenceResult) -> Option<Location> {
        let file_range = r.range();
        let uri = self.uri_for_file(file_range.file_id)?;
        let range = self.range_for_file(file_range.file_id, file_range.range)?;
        Some(Location { uri, range })
    }

    fn call_hierarchy_item_to_lsp(
        &self,
        item: &rua_analysis::CallHierarchyItem,
    ) -> Option<lsp_types::CallHierarchyItem> {
        let range = self.range_for_file(item.file_id, item.range)?;
        Some(lsp_types::CallHierarchyItem {
            detail: None,
            name: item.name.clone(),
            kind: lsp_types::SymbolKind::FUNCTION,
            uri: self.uri_for_file(item.file_id)?,
            range,
            selection_range: range,
            data: Some(hierarchy_data(item.project_id, item.target)),
            tags: None,
        })
    }

    fn type_hierarchy_item_to_lsp(
        &self,
        item: &rua_analysis::TypeHierarchyItem,
        kind: lsp_types::SymbolKind,
    ) -> Option<lsp_types::TypeHierarchyItem> {
        let range = self.range_for_file(item.file_id, item.range)?;
        Some(lsp_types::TypeHierarchyItem {
            detail: None,
            name: item.name.clone(),
            kind,
            uri: self.uri_for_file(item.file_id)?,
            range,
            selection_range: range,
            data: Some(hierarchy_data(item.project_id, item.target)),
            tags: None,
        })
    }

    fn publish_diagnostics(&mut self, uri: &Uri) {
        let Some(file_id) = self.file_id_for_uri(uri) else {
            return;
        };
        let analysis = self.host.analysis();
        let Some((source, line_index)) = self.source_line_index(file_id) else {
            return;
        };
        let native = self
            .project_file(file_id)
            .map_or_else(Vec::new, |file| analysis.diagnostics_in_project(file));

        let lsp_diags: Vec<Diagnostic> = native
            .iter()
            .map(|d| core_diag_to_lsp(d, &line_index, &source))
            .collect();

        self.send_diagnostics(uri, &lsp_diags, self.open_versions.get(&file_id).copied());
    }

    fn source_line_index(&self, file_id: FileId) -> Option<(Arc<str>, Arc<LineIndex>)> {
        let analysis = self.host.analysis();
        self.line_indices.borrow_mut().get(&analysis, file_id)
    }

    fn send_diagnostics(&self, uri: &Uri, diags: &[Diagnostic], version: Option<i32>) {
        let params = PublishDiagnosticsParams {
            uri: uri.clone(),
            diagnostics: diags.to_vec(),
            version,
        };
        let not = Notification::new(PublishDiagnostics::METHOD.to_string(), params);
        let _ = self.connection.sender.send(Message::Notification(not));
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
        document_on_type_formatting_provider: Some(lsp_types::DocumentOnTypeFormattingOptions {
            first_trigger_character: "\n".to_string(),
            more_trigger_character: None,
        }),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        selection_range_provider: Some(lsp_types::SelectionRangeProviderCapability::Simple(true)),
        call_hierarchy_provider: Some(lsp_types::CallHierarchyServerCapability::Simple(true)),
        code_lens_provider: Some(lsp_types::CodeLensOptions {
            resolve_provider: Some(false),
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        implementation_provider: Some(lsp_types::ImplementationProviderCapability::Simple(true)),
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
    let failed = match server.main_loop() {
        Ok(()) => false,
        Err(error) => {
            eprintln!("rua-lsp: error in main loop: {error}");
            true
        }
    };

    // Closing the connection sender is what lets the stdio writer terminate.
    // Keeping `server` alive across `join` deadlocks every graceful shutdown.
    drop(server);
    io_threads.join().expect("io threads join");
    eprintln!("rua-lsp: shutdown complete");
    if failed {
        std::process::exit(1);
    }
}

impl Server {
    fn index_workspace_folders(&mut self, folders: &[Uri]) {
        let roots = folders.iter().filter_map(uri_to_path).collect::<Vec<_>>();
        self.workspace_folders = roots.clone();
        if let Some((_, cancellation)) = self.workspace_scan.take() {
            cancellation.cancel();
        }
        // With no workspace folders, didOpen creates an ad-hoc project. An
        // asynchronous empty scan must not arrive later and erase that state.
        if roots.is_empty() {
            return;
        }
        let generation = self.next_scan_generation;
        self.next_scan_generation = self.next_scan_generation.wrapping_add(1);
        let cancellation = CancellationToken::new();
        self.workspace_scan = Some((generation, cancellation.clone()));
        let sender = self.background_sender.clone();
        if self
            .worker_pool
            .try_execute(move || {
                let scans = scan_workspace_roots(roots, &mut || cancellation.is_cancelled());
                let result = (!cancellation.is_cancelled()).then_some(scans);
                let _ = sender.send(BackgroundResult::WorkspaceScan { generation, result });
            })
            .is_err()
        {
            self.workspace_scan = None;
        }
    }

    fn apply_workspace_scan(&mut self, scans: Vec<WorkspaceScan>) {
        let mut change = Change::new();
        self.projects.clear();
        self.file_projects.clear();

        for scan in scans {
            let WorkspaceScan {
                project_index,
                root,
                files,
            } = scan;
            {
                let logical_base = normalize_physical_path(&root);
                let root_id = SourceRootId::new(self.next_root_id);
                self.next_root_id += 1;
                let project_id = ProjectId::new(project_index as u32);
                if files.is_empty() {
                    continue;
                }
                let root_file_index = files
                    .iter()
                    .position(|(path, _)| {
                        path.file_name().and_then(|name| name.to_str()) == Some("main.rua")
                    })
                    .unwrap_or(0);
                let root_file = self.ensure_file_id_for_path(&files[root_file_index].0);
                self.projects.insert(
                    project_id,
                    WorkspaceProject {
                        root_file,
                        workspace_roots: vec![ProjectRoot::new(root_id, logical_base)],
                    },
                );
                change.set_source_root(root_id, SourceRootKind::Workspace);
                for (path, text) in &files {
                    let file_id = self.ensure_file_id_for_path(path);
                    self.file_projects.insert(file_id, project_id);
                    self.disk_files.insert(
                        file_id,
                        DiskFile {
                            path: path.clone(),
                            analysis_path: path.clone(),
                            text: text.clone(),
                            source_root: root_id,
                            kind: FileKind::Source,
                        },
                    );
                    change.set_file_with_path(file_id, root_id, FileKind::Source, path, &**text);
                }
                eprintln!(
                    "rua-lsp: indexed {} source(s) under {}",
                    files.len(),
                    root.display()
                );
            }
        }
        self.rebuild_project_dependency_roots();
        self.set_project_changes(&mut change);
        self.apply_analysis_change(change);
    }
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

/// Exact semantic declaration target used by completion resolve.
fn make_resolve_data(target: Option<rua_analysis::FileRange>) -> Option<serde_json::Value> {
    let target = target?;
    Some(serde_json::json!({
        "file_id": target.file_id.index(),
        "start": target.range.start(),
        "end": target.range.end(),
    }))
}

fn parse_resolve_data(value: &serde_json::Value) -> Option<rua_analysis::FileRange> {
    let file_id = FileId::new(value.get("file_id")?.as_u64()? as u32);
    let start = value.get("start")?.as_u64()? as u32;
    let end = value.get("end")?.as_u64()? as u32;
    Some(rua_analysis::FileRange::new(
        file_id,
        TextRange::new(start, end),
    ))
}

fn hierarchy_data(project_id: ProjectId, target: DefId) -> serde_json::Value {
    serde_json::json!({
        "project": project_id.index(),
        "target": target.index(),
    })
}

fn hierarchy_identity(data: Option<&serde_json::Value>) -> Option<(ProjectId, DefId)> {
    let data = data?;
    let project = u32::try_from(data.get("project")?.as_u64()?).ok()?;
    let target = u32::try_from(data.get("target")?.as_u64()?).ok()?;
    Some((ProjectId::new(project), DefId::from_index(target)))
}

fn completion_to_lsp(
    item: &rua_analysis::CompletionItem,
    line_index: &LineIndex,
    source: &str,
    _file_id: rua_analysis::FileId,
) -> CompletionItem {
    let kind = match item.kind() {
        CompletionKind::Keyword => Some(CompletionItemKind::KEYWORD),
        CompletionKind::Variable | CompletionKind::Parameter => Some(CompletionItemKind::VARIABLE),
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
        Some(CompletionInsert::MacroCall { name, delimiter }) => {
            let snippet = match delimiter {
                MacroDelimiter::Parentheses => format!("{name}!($0)"),
                MacroDelimiter::Brackets => format!("{name}![$0]"),
                MacroDelimiter::Braces => format!("{name}!{{$0}}"),
            };
            (Some(snippet), Some(InsertTextFormat::SNIPPET))
        }
        Some(CompletionInsert::Plain(text)) => {
            (Some(text.clone()), Some(InsertTextFormat::PLAIN_TEXT))
        }
        Some(CompletionInsert::Snippet(text)) => {
            (Some(text.clone()), Some(InsertTextFormat::SNIPPET))
        }
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
    let replacement_text = insert_text
        .clone()
        .unwrap_or_else(|| item.label().to_string());
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
            new_text: replacement_text.clone(),
        }))
    });

    // label_details: structured display.  VS Code renders
    // label_details.detail directly after the label with no automatic
    // separator.  We always include the detail with a prefix that
    // provides visual separation: ": " for type-like kinds, " " for
    // signatures and keywords.  (When label_details is Some, VS Code
    // ignores the top-level detail field, so we MUST include it here.)
    let label_details = {
        let raw_detail = item.detail().map(|d| d.to_string());
        let detail = raw_detail.map(|d| match item.kind() {
            CompletionKind::Field
            | CompletionKind::Variable
            | CompletionKind::Parameter
            | CompletionKind::Variant
            | CompletionKind::Struct
            | CompletionKind::Enum
            | CompletionKind::Trait
            | CompletionKind::BuiltinType
            | CompletionKind::TypeAlias
            | CompletionKind::Module => format!(": {d}"),
            CompletionKind::Method
            | CompletionKind::Function
            | CompletionKind::Keyword
            | CompletionKind::Impl
            | CompletionKind::Macro => format!(" {d}"),
        });
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

    // Exact declaration target for semantic completion resolve.
    let data = make_resolve_data(item.target());

    CompletionItem {
        label: item.label().to_string(),
        kind,
        // detail is provided through label_details.detail with proper
        // separators; avoid duplication with the top-level field.
        detail: None,
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
    let (start_line, start_column) = line_index.line_col(range.start() as usize, source);
    let (end_line, end_column) = line_index.line_col(range.end() as usize, source);
    let code_str = diagnostic.code().map(|c| c.error_code().to_string());
    let severity = match diagnostic.code() {
        Some(c) => match c.severity() {
            rua_analysis::DiagnosticSeverity::Error => Some(DiagnosticSeverity::ERROR),
            rua_analysis::DiagnosticSeverity::Warning => Some(DiagnosticSeverity::WARNING),
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
    change: &SourceChange,
    source_for: impl Fn(FileId) -> Option<(Arc<str>, Arc<LineIndex>)>,
    uri_for: impl Fn(FileId) -> Option<Uri>,
) -> Option<WorkspaceEdit> {
    let mut edits = HashMap::new();
    for file_edit in change.file_edits() {
        let Some((source, line_index)) = source_for(file_edit.file_id()) else {
            continue;
        };
        let text_edits: Vec<TextEdit> = file_edit
            .edits()
            .iter()
            .map(|edit| {
                let (sl, sc) = line_index.line_col(edit.range().start() as usize, &source);
                let (el, ec) = line_index.line_col(edit.range().end() as usize, &source);
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
        DefKind::Chunk => LspSymbolKind::MODULE,
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_test_dir(label: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("rua-lsp-{label}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn finish_background_scans(server: &mut Server) {
        while server.workspace_scan.is_some() || server.library_scan.is_some() {
            let result = server
                .background_receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("background scan must finish");
            server.handle_background_result(result);
        }
    }

    #[test]
    fn file_uri_round_trips_reserved_and_unicode_path_bytes() {
        let temp = temp_test_dir("uri");
        let path = temp.join("100% #? 中.rua");
        std::fs::write(&path, "").unwrap();
        let uri = path_to_uri(&path).unwrap();
        assert!(uri.as_str().contains("%25"));
        assert!(uri.as_str().contains("%23"));
        assert!(uri.as_str().contains("%3F"));
        assert_eq!(
            normalize_physical_path(&uri_to_path(&uri).unwrap()),
            normalize_physical_path(&path)
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn uri_to_path_rejects_non_file_and_ambiguous_suffixes() {
        let https: Uri = "https://example.com/main.rua".parse().unwrap();
        let query: Uri = "file:///workspace/main.rua?generated=true".parse().unwrap();
        let fragment: Uri = "file:///workspace/main.rua#selection".parse().unwrap();
        assert_eq!(uri_to_path(&https), None);
        assert_eq!(uri_to_path(&query), None);
        assert_eq!(uri_to_path(&fragment), None);
    }

    #[cfg(windows)]
    #[test]
    fn file_uri_round_trips_windows_drive_and_unc_paths() {
        for path in [
            PathBuf::from(r"C:\Rua Project\main.rua"),
            PathBuf::from(r"\\server\share\main.rua"),
        ] {
            let uri = path_to_uri(&path).unwrap();
            assert_eq!(uri_to_path(&uri).unwrap(), normalize_physical_path(&path));
        }
    }

    #[test]
    fn recursive_scans_skip_build_dirs_and_symlink_cycles() {
        let temp = temp_test_dir("scan");
        std::fs::write(temp.join("main.rua"), "").unwrap();
        for directory in ["target", "node_modules", ".git"] {
            let path = temp.join(directory);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("ignored.rua"), "").unwrap();
            std::fs::write(path.join("ignored.ruai"), "").unwrap();
        }
        std::fs::write(
            temp.join(".ruaignore"),
            "generated/\nignored-*.rua\nignored-*.ruai\n",
        )
        .unwrap();
        std::fs::create_dir_all(temp.join("generated")).unwrap();
        std::fs::write(temp.join("generated/generated.rua"), "").unwrap();
        std::fs::write(temp.join("generated/generated.ruai"), "").unwrap();
        std::fs::write(temp.join("ignored-local.rua"), "").unwrap();
        std::fs::write(temp.join("ignored-library.ruai"), "").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&temp, temp.join("cycle")).unwrap();

        let mut workspace = Vec::new();
        scan_workspace_files(&temp, &mut |path, _| workspace.push(path.to_path_buf()));
        assert_eq!(
            workspace,
            vec![normalize_physical_path(&temp.join("main.rua"))]
        );

        let mut library = Vec::new();
        std::fs::write(temp.join("api.ruai"), "").unwrap();
        scan_library_root(&temp, &mut library);
        assert_eq!(library.len(), 1);
        assert_eq!(
            library[0].0,
            normalize_physical_path(&temp.join("api.ruai"))
        );
        std::fs::remove_dir_all(temp).unwrap();
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
            (
                SemanticTokenKind::EnumMember,
                SemanticTokenType::ENUM_MEMBER,
            ),
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
        server.open_document(uri.clone(), 1, "fn main() {}".to_string());
        // Should not panic
        server.publish_diagnostics(&uri);
    }

    #[test]
    fn overlay_versions_are_monotonic_and_close_restores_disk_text() {
        let uri: Uri = "file:///workspace/main.rua".parse().unwrap();
        let (server_connection, _client_connection) = lsp_server::Connection::memory();
        let mut server = Server::new(server_connection);
        let file_id = server.ensure_file_id(&uri);
        let path = Server::doc_key(&uri);
        let root = SourceRootId::new(0);
        server.projects.insert(
            ProjectId::new(0),
            WorkspaceProject {
                root_file: file_id,
                workspace_roots: vec![ProjectRoot::new(root, "/workspace")],
            },
        );
        server.file_projects.insert(file_id, ProjectId::new(0));
        server.disk_files.insert(
            file_id,
            DiskFile {
                path: path.clone(),
                analysis_path: path.clone(),
                text: "let value = 1;".to_string(),
                source_root: root,
                kind: FileKind::Source,
            },
        );
        let mut disk = Change::new();
        disk.set_source_root(root, SourceRootKind::Workspace);
        disk.set_file_with_path(file_id, root, FileKind::Source, path, "let value = 1;");
        server.set_project_changes(&mut disk);
        server.host.apply_change(disk);

        server.open_document(uri.clone(), 3, "let value = 2;".to_string());
        server.change_document(uri.clone(), 4, "let value = 3;".to_string());
        server.change_document(uri.clone(), 3, "let value = 99;".to_string());
        assert_eq!(
            server
                .host
                .analysis()
                .parse(file_id)
                .syntax_node()
                .text()
                .to_string(),
            "let value = 3;"
        );

        server.close_document(uri.clone());
        assert_eq!(server.file_id_for_uri(&uri), Some(file_id));
        assert_eq!(
            server
                .host
                .analysis()
                .parse(file_id)
                .syntax_node()
                .text()
                .to_string(),
            "let value = 1;"
        );
    }

    #[test]
    fn changes_for_unknown_or_closed_documents_do_not_create_inputs() {
        let uri: Uri = "file:///workspace/scratch.rua".parse().unwrap();
        let (server_connection, _client_connection) = lsp_server::Connection::memory();
        let mut server = Server::new(server_connection);

        server.change_document(uri.clone(), 1, "let ignored = true;".to_string());
        assert!(server.file_id_for_uri(&uri).is_none());

        server.open_document(uri.clone(), 1, "let temporary = true;".to_string());
        let file_id = server.file_id_for_uri(&uri).unwrap();
        server.close_document(uri.clone());
        server.change_document(uri.clone(), 2, "let ignored = false;".to_string());
        assert_eq!(server.file_id_for_uri(&uri), Some(file_id));
        assert!(server.host.analysis().file_kind(file_id).is_none());
    }

    #[test]
    fn library_reload_replaces_roots_and_clears_removed_definitions() {
        let temp = temp_test_dir("library-reload");
        let first = temp.join("first");
        let second = temp.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("api.ruai"), "pub fn old_api();").unwrap();
        std::fs::write(second.join("api.ruai"), "pub fn new_api();").unwrap();

        let (server_connection, _client_connection) = lsp_server::Connection::memory();
        let mut server = Server::new(server_connection);
        let main: Uri = "file:///workspace/main.rua".parse().unwrap();
        server.open_document(main, 1, "mod api;".to_string());

        server.reload_configuration(&serde_json::json!({
            "rua": { "library": [first.to_string_lossy()] }
        }));
        finish_background_scans(&mut server);
        let old_root = server.library_source_root.unwrap();
        let old_file = *server.library_file_ids.iter().next().unwrap();
        assert!(
            server
                .host
                .analysis()
                .def_map_for_project(ProjectId::new(0))
                .unwrap()
                .definitions()
                .any(|definition| definition.name() == "old_api")
        );

        server.reload_configuration(&serde_json::json!({
            "rua": { "library": [second.to_string_lossy()] }
        }));
        finish_background_scans(&mut server);
        let analysis = server.host.analysis();
        let definitions = analysis.def_map_for_project(ProjectId::new(0)).unwrap();
        assert!(
            !definitions
                .definitions()
                .any(|definition| definition.name() == "old_api")
        );
        assert!(
            definitions
                .definitions()
                .any(|definition| definition.name() == "new_api")
        );
        assert_eq!(analysis.source_root_kind(old_file), None);
        assert_ne!(server.library_source_root, Some(old_root));

        server.reload_configuration(&serde_json::json!({
            "rua": { "library": [] }
        }));
        finish_background_scans(&mut server);
        assert!(server.project_dependency_roots.is_empty());
        assert!(server.library_file_ids.is_empty());
        assert!(
            !server
                .host
                .analysis()
                .def_map_for_project(ProjectId::new(0))
                .unwrap()
                .definitions()
                .any(|definition| definition.name() == "new_api")
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn newer_library_scan_cancels_and_supersedes_older_generation() {
        let temp = temp_test_dir("library-scan-generation");
        let first = temp.join("first");
        let second = temp.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("api.ruai"), "pub fn stale_api();").unwrap();
        std::fs::write(second.join("api.ruai"), "pub fn current_api();").unwrap();

        let (server_connection, _client_connection) = lsp_server::Connection::memory();
        let mut server = Server::new(server_connection);
        server.open_document(
            "file:///workspace/main.rua".parse().unwrap(),
            1,
            "mod api;".to_string(),
        );
        server.reload_configuration(&serde_json::json!({
            "rua": { "library": [first] }
        }));
        server.reload_configuration(&serde_json::json!({
            "rua": { "library": [second] }
        }));
        finish_background_scans(&mut server);

        let definitions = server
            .host
            .analysis()
            .def_map_for_project(ProjectId::new(0))
            .unwrap();
        assert!(
            definitions
                .definitions()
                .any(|definition| definition.name() == "current_api")
        );
        assert!(
            !definitions
                .definitions()
                .any(|definition| definition.name() == "stale_api")
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn library_mount_name_is_the_logical_module_name() {
        let temp = temp_test_dir("library-mount");
        let declaration = temp.join("actual-name.ruai");
        std::fs::write(&declaration, "pub fn mounted_api();").unwrap();
        let (server_connection, _client_connection) = lsp_server::Connection::memory();
        let mut server = Server::new(server_connection);
        let main: Uri = "file:///workspace/main.rua".parse().unwrap();
        server.open_document(main, 1, "mod host;".to_string());
        server.reload_configuration(&serde_json::json!({
            "rua": { "libraryMounts": { "host": declaration.to_string_lossy() } }
        }));
        finish_background_scans(&mut server);

        assert!(
            server
                .host
                .analysis()
                .def_map_for_project(ProjectId::new(0))
                .unwrap()
                .definitions()
                .any(|definition| definition.name() == "mounted_api")
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn consecutive_watcher_batches_are_all_applied_to_the_current_root() {
        let temp = temp_test_dir("watcher-batches");
        let library = temp.join("library");
        std::fs::create_dir_all(&library).unwrap();
        let (server_connection, _client_connection) = lsp_server::Connection::memory();
        let mut server = Server::new(server_connection);
        let main: Uri = "file:///workspace/main.rua".parse().unwrap();
        server.open_document(main, 1, "mod one; mod two;".to_string());
        server.reload_configuration(&serde_json::json!({
            "rua": { "library": [library.to_string_lossy()] }
        }));
        finish_background_scans(&mut server);
        let root = server.library_source_root;

        for (name, declaration) in [
            ("one", "pub fn first_api();"),
            ("two", "pub fn second_api();"),
        ] {
            let path = library.join(format!("{name}.ruai"));
            std::fs::write(&path, declaration).unwrap();
            server.handle_watched_file_change(&DidChangeWatchedFilesParams {
                changes: vec![lsp_types::FileEvent {
                    uri: path_to_uri(&path).unwrap(),
                    typ: lsp_types::FileChangeType::CREATED,
                }],
            });
        }

        assert_eq!(server.library_source_root, root);
        let analysis = server.host.analysis();
        let main_id = server
            .file_id_for_uri(&"file:///workspace/main.rua".parse().unwrap())
            .unwrap();
        let one_id = server.ensure_file_id_for_path(&library.join("one.ruai"));
        let resolution = analysis.resolve_module_in_project(ProjectId::new(0), main_id, "one");
        let one_path = analysis.file_path(one_id).cloned();
        let definitions = analysis.def_map_for_project(ProjectId::new(0)).unwrap();
        let names: Vec<_> = definitions
            .definitions()
            .map(|definition| definition.name().to_string())
            .collect();
        assert!(
            definitions
                .definitions()
                .any(|definition| definition.name() == "first_api"),
            "{names:?}; resolution={resolution:?}; one={one_id:?} path={one_path:?}; roots={:?}",
            server.project_dependency_roots
        );
        assert!(
            definitions
                .definitions()
                .any(|definition| definition.name() == "second_api"),
            "{names:?}"
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn watcher_registration_responses_update_active_and_failure_state() {
        let temp = temp_test_dir("watcher-response-state");
        let library = temp.join("library");
        std::fs::create_dir_all(&library).unwrap();
        let (server_connection, client_connection) = lsp_server::Connection::memory();
        let mut server = Server::new(server_connection);

        server.reload_configuration(&serde_json::json!({
            "rua": { "library": [library.to_string_lossy()] }
        }));
        finish_background_scans(&mut server);
        let Message::Request(register) = client_connection.receiver.recv().unwrap() else {
            panic!("expected register request");
        };
        assert_eq!(register.method, "client/registerCapability");
        let registration_id = server.watch_registration_id.clone().unwrap();
        assert!(!server.watch_registrations[&registration_id].is_active());

        server.handle_response(Response::new_ok(register.id, ()));
        assert!(server.watch_registrations[&registration_id].is_active());

        server.reload_configuration(&serde_json::json!({
            "rua": { "library": [] }
        }));
        finish_background_scans(&mut server);
        let Message::Request(unregister) = client_connection.receiver.recv().unwrap() else {
            panic!("expected unregister request");
        };
        assert_eq!(unregister.method, "client/unregisterCapability");
        server.handle_response(Response::new_err(
            unregister.id,
            lsp_server::ErrorCode::InternalError as i32,
            "client refused unregistration".to_string(),
        ));

        assert!(server.watch_registrations[&registration_id].is_active());
        assert_eq!(
            server.last_watch_failure,
            Some(WatchRegistrationFailure {
                operation: WatchOperation::Unregister,
                registration_id,
                code: lsp_server::ErrorCode::InternalError as i32,
                message: "client refused unregistration".to_string(),
            })
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn watcher_registration_handles_unregister_response_before_register_response() {
        let temp = temp_test_dir("watcher-response-order");
        let library = temp.join("library");
        std::fs::create_dir_all(&library).unwrap();
        let (server_connection, client_connection) = lsp_server::Connection::memory();
        let mut server = Server::new(server_connection);

        server.reload_configuration(&serde_json::json!({
            "rua": { "library": [library.to_string_lossy()] }
        }));
        finish_background_scans(&mut server);
        let Message::Request(register) = client_connection.receiver.recv().unwrap() else {
            panic!("expected register request");
        };
        let registration_id = server.watch_registration_id.clone().unwrap();

        server.reload_configuration(&serde_json::json!({
            "rua": { "library": [] }
        }));
        finish_background_scans(&mut server);
        let Message::Request(unregister) = client_connection.receiver.recv().unwrap() else {
            panic!("expected unregister request");
        };
        server.handle_response(Response::new_ok(unregister.id, ()));
        assert!(server.watch_registrations.contains_key(&registration_id));

        server.handle_response(Response::new_ok(register.id, ()));
        assert!(!server.watch_registrations.contains_key(&registration_id));
        assert!(server.pending_watch_requests.is_empty());
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn workspace_folders_build_separate_projects_and_definition_identities() {
        let temp = temp_test_dir("multi-root");
        let first = temp.join("first");
        let second = temp.join("second");
        for (folder, marker) in [(&first, "first_only"), (&second, "second_only")] {
            std::fs::create_dir_all(folder).unwrap();
            std::fs::write(folder.join("main.rua"), "mod shared; shared::same();").unwrap();
            std::fs::write(
                folder.join("shared.rua"),
                format!("pub fn same() {{}} pub fn {marker}() {{}}"),
            )
            .unwrap();
        }

        let (server_connection, _client_connection) = lsp_server::Connection::memory();
        let mut server = Server::new(server_connection);
        server.index_workspace_folders(&[
            path_to_uri(&first).unwrap(),
            path_to_uri(&second).unwrap(),
        ]);
        finish_background_scans(&mut server);
        assert_eq!(server.projects.len(), 2);

        let analysis = server.host.analysis();
        let first_map = analysis.def_map_for_project(ProjectId::new(0)).unwrap();
        let second_map = analysis.def_map_for_project(ProjectId::new(1)).unwrap();
        assert!(
            first_map
                .definitions()
                .any(|definition| definition.name() == "first_only")
        );
        assert!(
            !first_map
                .definitions()
                .any(|definition| definition.name() == "second_only")
        );
        assert!(
            second_map
                .definitions()
                .any(|definition| definition.name() == "second_only")
        );
        let first_same = first_map
            .definitions()
            .find(|definition| definition.name() == "same")
            .unwrap()
            .id();
        let second_same = second_map
            .definitions()
            .find(|definition| definition.name() == "same")
            .unwrap()
            .id();
        assert_ne!(first_same, second_same);

        let first_main = server
            .file_id_for_uri(&path_to_uri(&first.join("main.rua")).unwrap())
            .unwrap();
        let second_main = server
            .file_id_for_uri(&path_to_uri(&second.join("main.rua")).unwrap())
            .unwrap();
        assert_eq!(
            server.project_id_for_file(first_main),
            Some(ProjectId::new(0))
        );
        assert_eq!(
            server.project_id_for_file(second_main),
            Some(ProjectId::new(1))
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn workspace_settings_keep_library_roots_project_scoped() {
        let temp = temp_test_dir("multi-root-libraries");
        let first = temp.join("first");
        let second = temp.join("second");
        let first_library = first.join("types");
        let second_library = second.join("types");
        for (workspace, library, api) in [
            (&first, &first_library, "pub fn first_api();"),
            (&second, &second_library, "pub fn second_api();"),
        ] {
            std::fs::create_dir_all(library).unwrap();
            std::fs::write(workspace.join("main.rua"), "mod api;").unwrap();
            std::fs::write(library.join("api.ruai"), api).unwrap();
        }

        let (server_connection, _client_connection) = lsp_server::Connection::memory();
        let mut server = Server::new(server_connection);
        server.index_workspace_folders(&[
            path_to_uri(&first).unwrap(),
            path_to_uri(&second).unwrap(),
        ]);
        finish_background_scans(&mut server);
        server.reload_configuration(&serde_json::json!({
            "rua": {
                "workspaceSettings": [
                    { "projectIndex": 0, "library": [first_library] },
                    { "projectIndex": 1, "library": [second_library] }
                ]
            }
        }));
        finish_background_scans(&mut server);

        let analysis = server.host.analysis();
        let first_map = analysis.def_map_for_project(ProjectId::new(0)).unwrap();
        let second_map = analysis.def_map_for_project(ProjectId::new(1)).unwrap();
        assert!(
            first_map
                .definitions()
                .any(|definition| definition.name() == "first_api")
        );
        assert!(
            !first_map
                .definitions()
                .any(|definition| definition.name() == "second_api")
        );
        assert!(
            second_map
                .definitions()
                .any(|definition| definition.name() == "second_api")
        );
        assert!(
            !second_map
                .definitions()
                .any(|definition| definition.name() == "first_api")
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn server_starts_with_empty_state() {
        let (connection, _client) = lsp_server::Connection::memory();
        let server = Server::new(connection);
        assert!(server.file_ids.is_empty());
        assert!(server.open_buffers.is_empty());
        assert!(server.open_versions.is_empty());
        assert!(server.disk_files.is_empty());
    }

    #[test]
    fn cancelled_background_query_returns_request_cancelled() {
        let (connection, client) = lsp_server::Connection::memory();
        let mut server = Server::new(connection);
        let request_id = RequestId::from(7);
        let cancellation = CancellationToken::new();
        server.pending_queries.insert(
            1,
            PendingQuery {
                request_id: request_id.clone(),
                input_generation: 0,
                cancellation,
            },
        );

        server.cancel_request(request_id.clone());
        server.handle_background_result(BackgroundResult::References {
            task_id: 1,
            result: Some(Vec::new()),
        });

        let Message::Response(response) = client.receiver.recv().unwrap() else {
            panic!("expected cancellation response");
        };
        assert_eq!(response.id, request_id);
        assert_eq!(
            response.error.unwrap().code,
            lsp_server::ErrorCode::RequestCanceled as i32
        );
    }

    #[test]
    fn changed_input_rejects_stale_background_result() {
        let (connection, client) = lsp_server::Connection::memory();
        let mut server = Server::new(connection);
        let request_id = RequestId::from(8);
        server.pending_queries.insert(
            2,
            PendingQuery {
                request_id: request_id.clone(),
                input_generation: 0,
                cancellation: CancellationToken::new(),
            },
        );
        server.input_generation = 1;

        server.handle_background_result(BackgroundResult::References {
            task_id: 2,
            result: Some(Vec::new()),
        });

        let Message::Response(response) = client.receiver.recv().unwrap() else {
            panic!("expected stale response rejection");
        };
        assert_eq!(response.id, request_id);
        assert_eq!(
            response.error.unwrap().code,
            lsp_server::ErrorCode::ContentModified as i32
        );
    }

    #[test]
    fn line_index_cache_reuses_revision_and_rebuilds_after_change() {
        let (connection, _client) = lsp_server::Connection::memory();
        let mut server = Server::new(connection);
        let uri: Uri = "file:///workspace/main.rua".parse().unwrap();
        server.open_document(uri.clone(), 1, "fn before() {}\n".into());
        let file_id = server.file_id_for_uri(&uri).unwrap();

        let (first_source, first_index) = server.source_line_index(file_id).unwrap();
        let (same_source, same_index) = server.source_line_index(file_id).unwrap();
        assert!(Arc::ptr_eq(&first_source, &same_source));
        assert!(Arc::ptr_eq(&first_index, &same_index));

        server.change_document(uri, 2, "fn after() {}\n".into());
        let (changed_source, changed_index) = server.source_line_index(file_id).unwrap();
        assert_eq!(&*changed_source, "fn after() {}\n");
        assert!(!Arc::ptr_eq(&first_source, &changed_source));
        assert!(!Arc::ptr_eq(&first_index, &changed_index));
    }

    // -- completion_to_lsp unit tests ---------------------------------------

    fn empty_line_index() -> LineIndex {
        LineIndex::new("")
    }

    #[test]
    fn sort_text_higher_relevance_sorts_first() {
        // Locals (relevance 95) should sort before keywords (relevance 50).
        let local =
            rua_analysis::CompletionItem::new("my_var", rua_analysis::CompletionKind::Variable)
                .with_detail("my_var: i64")
                .with_relevance(rua_analysis::CompletionRelevance::local(0));
        let keyword =
            rua_analysis::CompletionItem::new("fn", rua_analysis::CompletionKind::Keyword)
                .with_detail("keyword fn")
                .with_relevance(rua_analysis::CompletionRelevance::keyword());

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
            .with_relevance(rua_analysis::CompletionRelevance::path_member());
        let b = rua_analysis::CompletionItem::new("beta", rua_analysis::CompletionKind::Variable)
            .with_relevance(rua_analysis::CompletionRelevance::path_member());

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
        assert_eq!(
            lsp.insert_text_format,
            Some(lsp_types::InsertTextFormat::SNIPPET)
        );
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
        assert_eq!(
            lsp.insert_text_format,
            Some(lsp_types::InsertTextFormat::SNIPPET)
        );
    }

    #[test]
    fn function_completion_with_params_has_placeholder_snippets() {
        let f =
            rua_analysis::CompletionItem::new("translate", rua_analysis::CompletionKind::Method)
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
        assert_eq!(
            lsp.insert_text_format,
            Some(lsp_types::InsertTextFormat::SNIPPET)
        );
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
        let item =
            rua_analysis::CompletionItem::new("my_var", rua_analysis::CompletionKind::Variable)
                .with_replacement_range(rua_analysis::TextRange::new(5, 7));

        let source = "abc def ghi";
        // replacement range 5..7 = "de"
        let li = LineIndex::new(source);
        let lsp = completion_to_lsp(&item, &li, source, FileId::new(0));
        assert!(
            lsp.text_edit.is_some(),
            "text_edit should be set when replacement_range is set"
        );
    }

    #[test]
    fn deprecated_item_has_deprecated_tag() {
        let item =
            rua_analysis::CompletionItem::new("old_fn", rua_analysis::CompletionKind::Function)
                .deprecated(true);

        let li = empty_line_index();
        let lsp = completion_to_lsp(&item, &li, "", FileId::new(0));
        assert_eq!(lsp.deprecated, Some(true));
        assert!(
            lsp.tags
                .is_some_and(|t| t.contains(&lsp_types::CompletionItemTag::DEPRECATED))
        );
    }

    #[test]
    fn normal_item_has_no_deprecated_tag() {
        let item =
            rua_analysis::CompletionItem::new("new_fn", rua_analysis::CompletionKind::Function);

        let li = empty_line_index();
        let lsp = completion_to_lsp(&item, &li, "", FileId::new(0));
        assert_eq!(lsp.deprecated, None);
        assert!(lsp.tags.is_none());
    }

    #[test]
    fn label_details_type_like_kinds_have_colon_prefix() {
        // Fields, variables, variants etc. get ": " prefix for visual separation.
        for kind in [
            rua_analysis::CompletionKind::Field,
            rua_analysis::CompletionKind::Variable,
            rua_analysis::CompletionKind::Parameter,
            rua_analysis::CompletionKind::Variant,
        ] {
            let item = rua_analysis::CompletionItem::new("x", kind).with_detail("i64");
            let li = empty_line_index();
            let lsp = completion_to_lsp(&item, &li, "", FileId::new(0));
            let ld = lsp
                .label_details
                .as_ref()
                .expect("label_details should be set");
            assert_eq!(
                ld.detail,
                Some(": i64".to_string()),
                "kind={kind:?}: detail should have ': ' prefix"
            );
        }
    }

    #[test]
    fn label_details_signature_kinds_have_space_prefix() {
        // Methods, functions, keywords get " " prefix for visual separation.
        for kind in [
            rua_analysis::CompletionKind::Function,
            rua_analysis::CompletionKind::Method,
            rua_analysis::CompletionKind::Keyword,
            rua_analysis::CompletionKind::Macro,
        ] {
            let item = rua_analysis::CompletionItem::new("greet", kind)
                .with_detail("fn greet(name: String) -> String");
            let li = empty_line_index();
            let lsp = completion_to_lsp(&item, &li, "", FileId::new(0));
            let ld = lsp
                .label_details
                .as_ref()
                .expect("label_details should be set");
            assert_eq!(
                ld.detail,
                Some(" fn greet(name: String) -> String".to_string()),
                "kind={kind:?}: detail should have ' ' prefix"
            );
        }
    }

    #[test]
    fn label_details_top_level_detail_is_none() {
        // label_details.detail supersedes the top-level detail field.
        let item = rua_analysis::CompletionItem::new("x", rua_analysis::CompletionKind::Field)
            .with_detail("i64");
        let li = empty_line_index();
        let lsp = completion_to_lsp(&item, &li, "", FileId::new(0));
        assert!(
            lsp.detail.is_none(),
            "top-level detail should be None when label_details is present"
        );
    }

    #[test]
    fn label_details_postfix_template_has_space_prefix() {
        // Postfix completions (.if, .match, etc.) use Keyword kind.
        let item = rua_analysis::CompletionItem::new(".if", rua_analysis::CompletionKind::Keyword)
            .with_detail("if expr { … }");
        let li = empty_line_index();
        let lsp = completion_to_lsp(&item, &li, "", FileId::new(0));
        let ld = lsp
            .label_details
            .as_ref()
            .expect("label_details should be set");
        assert_eq!(
            ld.detail,
            Some(" if expr { … }".to_string()),
            "postfix detail should have ' ' prefix, got {:?}",
            ld.detail
        );
        assert_eq!(ld.description, Some("keyword".to_string()));
    }
}
