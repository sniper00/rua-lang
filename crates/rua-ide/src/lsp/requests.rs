use super::*;

impl Server {
    pub(super) fn handle_request(&mut self, request: Request) {
        match request.method.as_str() {
            Formatting::METHOD => self.handle_formatting(request),
            RangeFormatting::METHOD => self.handle_range_formatting(request),
            SelectionRangeRequest::METHOD => self.handle_selection_range(request),
            CodeLensRequest::METHOD => self.handle_code_lens(request),
            HoverRequest::METHOD => self.handle_hover(request),
            GotoDefinition::METHOD => self.handle_definition(request),
            GotoImplementation::METHOD => self.handle_goto_implementation(request),
            DocumentSymbolRequest::METHOD => self.handle_document_symbol(request),
            Completion::METHOD => self.handle_completion(request),
            References::METHOD => self.handle_references(request),
            Rename::METHOD => self.handle_rename(request),
            PrepareRenameRequest::METHOD => self.handle_prepare_rename(request),
            SemanticTokensFullRequest::METHOD => self.handle_semantic_tokens(request),
            SemanticTokensRangeRequest::METHOD => self.handle_semantic_tokens_range(request),
            ResolveCompletionItem::METHOD => self.handle_resolve_completion(request),
            SignatureHelpRequest::METHOD => self.handle_signature_help(request),
            InlayHintRequest::METHOD => self.handle_inlay_hint(request),
            DocumentHighlightRequest::METHOD => self.handle_document_highlight(request),
            CodeActionRequest::METHOD => self.handle_code_action(request),
            ExecuteCommand::METHOD => self.handle_execute_command(request),
            CallHierarchyPrepare::METHOD => self.handle_call_hierarchy_prepare(request),
            CallHierarchyIncomingCalls::METHOD => self.handle_call_hierarchy_incoming(request),
            CallHierarchyOutgoingCalls::METHOD => self.handle_call_hierarchy_outgoing(request),
            TypeHierarchyPrepare::METHOD => self.handle_type_hierarchy_prepare(request),
            TypeHierarchySubtypes::METHOD => self.handle_type_hierarchy_subtypes(request),
            TypeHierarchySupertypes::METHOD => self.handle_type_hierarchy_supertypes(request),
            WorkspaceSymbolRequest::METHOD => self.handle_workspace_symbol(request),
            FoldingRangeRequest::METHOD => self.handle_folding_range(request),
            DocumentLinkRequest::METHOD => self.handle_document_link(request),
            OnTypeFormatting::METHOD => self.handle_on_type_formatting(request),
            _ => {
                let response = Response::new_err(
                    request.id,
                    lsp_server::ErrorCode::MethodNotFound as i32,
                    format!("unknown request: {}", request.method),
                );
                let _ = self.connection.sender.send(Message::Response(response));
            }
        }
    }
}
