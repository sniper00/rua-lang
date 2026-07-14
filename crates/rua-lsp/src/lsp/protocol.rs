use super::*;

impl Server {
    pub(super) fn main_loop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let incoming = self.connection.receiver.clone();
        let background = self.background_receiver.clone();
        loop {
            // Apply every already-queued protocol input before publishing a
            // worker result. Otherwise a stale query can race ahead of a
            // didChange that the client already sent.
            crossbeam_channel::select_biased! {
                recv(incoming) -> message => {
                    let message = match message {
                        Ok(message) => message,
                        Err(_) => return Ok(()),
                    };
                    match message {
                        Message::Request(request) => {
                            if request.method == Shutdown::METHOD {
                                self.begin_shutdown(request.id)?;
                                return Ok(());
                            }
                            self.handle_request(request);
                        }
                        Message::Notification(notification) => {
                            self.handle_notification(notification);
                        }
                        Message::Response(response) => self.handle_response(response),
                    }
                }
                recv(background) -> result => {
                    if let Ok(result) = result {
                        self.handle_background_result(result);
                    }
                }
            }
        }
    }

    fn begin_shutdown(&mut self, request_id: RequestId) -> Result<(), Box<dyn std::error::Error>> {
        self.connection
            .sender
            .send(Message::Response(Response::new_ok(request_id, ())))?;
        loop {
            match self.connection.receiver.recv() {
                Ok(Message::Notification(notification)) if notification.method == Exit::METHOD => {
                    return Ok(());
                }
                Ok(Message::Response(response)) => self.handle_response(response),
                Ok(Message::Request(request)) => {
                    let response = Response::new_err(
                        request.id,
                        lsp_server::ErrorCode::InvalidRequest as i32,
                        "server is shutting down".to_string(),
                    );
                    self.connection.sender.send(Message::Response(response))?;
                }
                Ok(Message::Notification(_)) => {}
                Err(_) => return Ok(()),
            }
        }
    }

    pub(super) fn cancel_request(&mut self, request_id: RequestId) {
        for pending in self.pending_queries.values() {
            if pending.request_id == request_id {
                pending.cancellation.cancel();
            }
        }
    }

    pub(super) fn handle_background_result(&mut self, result: BackgroundResult) {
        match result {
            BackgroundResult::WorkspaceScan { generation, result } => {
                let is_current = self
                    .workspace_scan
                    .as_ref()
                    .is_some_and(|(current, _)| *current == generation);
                if !is_current {
                    return;
                }
                let (_, cancellation) = self.workspace_scan.take().unwrap();
                if !cancellation.is_cancelled()
                    && let Some(scans) = result
                {
                    self.apply_workspace_scan(scans);
                }
                return;
            }
            BackgroundResult::LibraryScan { generation, result } => {
                let is_current = self
                    .library_scan
                    .as_ref()
                    .is_some_and(|(current, _)| *current == generation);
                if !is_current {
                    return;
                }
                let (_, cancellation) = self.library_scan.take().unwrap();
                if !cancellation.is_cancelled() {
                    match result {
                        Ok(config) => self.apply_library_config(config),
                        Err(error) if error != "library scan cancelled" => {
                            eprintln!("rua-lsp: library scan failed: {error}");
                        }
                        Err(_) => {}
                    }
                }
                return;
            }
            BackgroundResult::References { .. } | BackgroundResult::WorkspaceSymbols { .. } => {}
        }

        let task_id = match &result {
            BackgroundResult::References { task_id, .. }
            | BackgroundResult::WorkspaceSymbols { task_id, .. } => *task_id,
            BackgroundResult::WorkspaceScan { .. } | BackgroundResult::LibraryScan { .. } => {
                unreachable!("scan results return above")
            }
        };
        let Some(pending) = self.pending_queries.remove(&task_id) else {
            return;
        };
        if pending.cancellation.is_cancelled() {
            self.send_query_error(
                pending.request_id,
                lsp_server::ErrorCode::RequestCanceled,
                "request cancelled",
            );
            return;
        }
        if pending.input_generation != self.input_generation {
            self.send_query_error(
                pending.request_id,
                lsp_server::ErrorCode::ContentModified,
                "analysis input changed while the request was running",
            );
            return;
        }

        match result {
            BackgroundResult::References { result, .. } => {
                let Some(references) = result else {
                    self.send_query_error(
                        pending.request_id,
                        lsp_server::ErrorCode::RequestCanceled,
                        "request cancelled",
                    );
                    return;
                };
                let locations = references
                    .iter()
                    .filter_map(|reference| self.ref_to_location(reference))
                    .collect::<Vec<_>>();
                let response = Response::new_ok(pending.request_id, Some(locations));
                let _ = self.connection.sender.send(Message::Response(response));
            }
            BackgroundResult::WorkspaceSymbols { result, .. } => {
                let Some(mut semantic_symbols) = result else {
                    self.send_query_error(
                        pending.request_id,
                        lsp_server::ErrorCode::RequestCanceled,
                        "request cancelled",
                    );
                    return;
                };
                semantic_symbols.sort_by_key(|symbol| {
                    (
                        symbol.name().to_string(),
                        symbol.file_id(),
                        symbol.selection_range(),
                    )
                });
                semantic_symbols
                    .dedup_by_key(|symbol| (symbol.file_id(), symbol.selection_range()));
                let symbols = semantic_symbols
                    .into_iter()
                    .take(50)
                    .filter_map(|symbol| {
                        let uri = self.uri_for_file(symbol.file_id())?;
                        let range =
                            self.range_for_file(symbol.file_id(), symbol.selection_range())?;
                        Some(WorkspaceSymbol {
                            name: symbol.name().to_string(),
                            kind: to_lsp_symbol_kind(symbol.kind()),
                            location: OneOf::Left(Location { uri, range }),
                            container_name: symbol.container_name().map(str::to_string),
                            tags: None,
                            data: None,
                        })
                    })
                    .collect::<Vec<_>>();
                let response =
                    Response::new_ok(pending.request_id, WorkspaceSymbolResponse::Nested(symbols));
                let _ = self.connection.sender.send(Message::Response(response));
            }
            BackgroundResult::WorkspaceScan { .. } | BackgroundResult::LibraryScan { .. } => {
                unreachable!("scan results return above")
            }
        }
    }

    pub(super) fn send_query_error(
        &self,
        request_id: RequestId,
        code: lsp_server::ErrorCode,
        message: &str,
    ) {
        let response = Response::new_err(request_id, code as i32, message.to_string());
        let _ = self.connection.sender.send(Message::Response(response));
    }

    pub(super) fn handle_response(&mut self, response: Response) {
        let Some((operation, registration_id)) = self.pending_watch_requests.remove(&response.id)
        else {
            return;
        };
        let succeeded = response.error.is_none();
        let state = self
            .watch_registrations
            .entry(registration_id.clone())
            .or_default();
        match operation {
            WatchOperation::Register => state.register_result = Some(succeeded),
            WatchOperation::Unregister => state.unregister_result = Some(succeeded),
        }
        if let Some(error) = response.error {
            self.last_watch_failure = Some(WatchRegistrationFailure {
                operation,
                registration_id: registration_id.clone(),
                code: error.code,
                message: error.message,
            });
            if operation == WatchOperation::Register
                && self.watch_registration_id.as_deref() == Some(registration_id.as_str())
            {
                self.watch_registration_id = None;
            }
        }
        let can_remove = self
            .watch_registrations
            .get(&registration_id)
            .is_some_and(|state| {
                !state.desired
                    && state.register_result.is_some()
                    && state.unregister_result == Some(true)
            });
        if can_remove {
            self.watch_registrations.remove(&registration_id);
        }
    }
}
