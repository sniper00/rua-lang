use super::*;

impl Server {
    pub(super) fn handle_notification(&mut self, notification: Notification) {
        match notification.method.as_str() {
            Cancel::METHOD => {
                extract_notification!(
                    notification,
                    lsp_types::CancelParams,
                    "cancelRequest",
                    |params| {
                        let request_id = match params.id {
                            lsp_types::NumberOrString::Number(id) => RequestId::from(id),
                            lsp_types::NumberOrString::String(id) => RequestId::from(id),
                        };
                        self.cancel_request(request_id);
                    }
                );
            }
            DidOpenTextDocument::METHOD => {
                extract_notification!(
                    notification,
                    lsp_types::DidOpenTextDocumentParams,
                    "didOpen",
                    |params| {
                        self.open_document(
                            params.text_document.uri,
                            params.text_document.version,
                            params.text_document.text,
                        );
                    }
                );
            }
            DidChangeTextDocument::METHOD => {
                extract_notification!(
                    notification,
                    lsp_types::DidChangeTextDocumentParams,
                    "didChange",
                    |params| {
                        if let Some(change) = params.content_changes.last() {
                            self.change_document(
                                params.text_document.uri,
                                params.text_document.version,
                                change.text.clone(),
                            );
                        }
                    }
                );
            }
            DidCloseTextDocument::METHOD => {
                extract_notification!(
                    notification,
                    lsp_types::DidCloseTextDocumentParams,
                    "didClose",
                    |params| {
                        self.close_document(params.text_document.uri);
                    }
                );
            }
            DidSaveTextDocument::METHOD => {
                extract_notification!(
                    notification,
                    lsp_types::DidSaveTextDocumentParams,
                    "didSave",
                    |params| {
                        self.handle_did_save(&params);
                    }
                );
            }
            DidChangeConfiguration::METHOD => {
                extract_notification!(
                    notification,
                    lsp_types::DidChangeConfigurationParams,
                    "didChangeConfiguration",
                    |params| {
                        self.reload_configuration(&params.settings);
                    }
                );
            }
            DidChangeWatchedFiles::METHOD => {
                extract_notification!(
                    notification,
                    DidChangeWatchedFilesParams,
                    "didChangeWatchedFiles",
                    |params| {
                        self.handle_watched_file_change(&params);
                    }
                );
            }
            _ => {}
        }
    }
}
