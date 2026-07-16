#![cfg(feature = "lsp")]

use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

use serde_json::{Value, json};

struct ProtocolServer {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: Receiver<Value>,
    pending: VecDeque<Value>,
}

impl ProtocolServer {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rua-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start rua-lsp");
        let stdin = child.stdin.take().expect("language server stdin");
        let stdout = child.stdout.take().expect("language server stdout");
        let (sender, messages) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut content_length = None;
                loop {
                    let mut header = String::new();
                    match reader.read_line(&mut header) {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    if header == "\r\n" {
                        break;
                    }
                    if let Some(value) = header.strip_prefix("Content-Length:") {
                        content_length = value.trim().parse::<usize>().ok();
                    }
                }
                let Some(content_length) = content_length else {
                    return;
                };
                let mut body = vec![0; content_length];
                if reader.read_exact(&mut body).is_err() {
                    return;
                }
                let Ok(message) = serde_json::from_slice(&body) else {
                    return;
                };
                if sender.send(message).is_err() {
                    return;
                }
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            messages,
            pending: VecDeque::new(),
        }
    }

    fn send(&mut self, message: Value) {
        let body = serde_json::to_vec(&message).expect("serialize protocol message");
        let stdin = self.stdin.as_mut().expect("language server stdin is open");
        write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        stdin.write_all(&body).unwrap();
        stdin.flush().unwrap();
    }

    fn matching_message(&mut self, predicate: impl Fn(&Value) -> bool) -> Value {
        if let Some(index) = self.pending.iter().position(&predicate) {
            return self.pending.remove(index).expect("queued protocol message");
        }
        loop {
            let message = self
                .messages
                .recv_timeout(Duration::from_secs(20))
                .unwrap_or_else(|error| panic!("timed out waiting for protocol message: {error}"));
            if predicate(&message) {
                return message;
            }
            self.pending.push_back(message);
        }
    }

    fn response(&mut self, id: i64) -> Value {
        self.matching_message(|message| {
            message.get("id").and_then(Value::as_i64) == Some(id)
                && (message.get("result").is_some() || message.get("error").is_some())
        })
    }

    fn server_request(&mut self, method: &str) -> Value {
        self.matching_message(|message| {
            message.get("method").and_then(Value::as_str) == Some(method)
                && message.get("id").is_some()
        })
    }

    fn respond_ok(&mut self, request: &Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": request["id"].clone(),
            "result": null
        }));
    }

    fn initialize(&mut self) {
        self.initialize_with(Vec::new(), Value::Null);
    }

    fn initialize_with(&mut self, workspace_folders: Vec<Value>, initialization_options: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": null,
                "capabilities": {},
                "workspaceFolders": workspace_folders,
                "initializationOptions": initialization_options
            }
        }));
        let response = self.response(1);
        assert!(response.get("result").is_some(), "{response}");
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        }));
    }

    fn shutdown(mut self) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "shutdown",
            "params": null
        }));
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "exit",
            "params": null
        }));
        let response = self.response(99);
        assert_eq!(response.get("result"), Some(&Value::Null));
        self.stdin.take();
        let status = self.child.wait().expect("wait for rua-lsp");
        assert!(status.success(), "rua-lsp exited with {status}");
    }
}

fn temp_test_dir(label: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rua-lsp-protocol-{label}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn file_uri(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    url::Url::from_file_path(canonical)
        .expect("absolute test path")
        .to_string()
}

fn position_of(source: &str, needle: &str) -> Value {
    let offset = source.find(needle).expect("needle in source");
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    json!({ "line": line, "character": source[line_start..offset].encode_utf16().count() })
}

fn position_after(source: &str, needle: &str) -> Value {
    let offset = source.find(needle).expect("needle in source") + needle.len();
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    json!({ "line": line, "character": source[line_start..offset].encode_utf16().count() })
}

fn offset_of_position(source: &str, position: &Value) -> usize {
    let line = position["line"].as_u64().unwrap() as usize;
    let character = position["character"].as_u64().unwrap() as usize;
    let line_start = if line == 0 {
        0
    } else {
        source
            .match_indices('\n')
            .nth(line - 1)
            .map(|(offset, _)| offset + 1)
            .expect("line in source")
    };
    let line_end = source[line_start..]
        .find('\n')
        .map_or(source.len(), |offset| line_start + offset);
    let line_text = &source[line_start..line_end];
    let mut utf16 = 0;
    for (offset, character_value) in line_text.char_indices() {
        if utf16 == character {
            return line_start + offset;
        }
        utf16 += character_value.len_utf16();
    }
    assert_eq!(utf16, character, "position outside line: {position}");
    line_end
}

fn apply_single_edit(source: &str, edit: &Value) -> String {
    let start = offset_of_position(source, &edit["range"]["start"]);
    let end = offset_of_position(source, &edit["range"]["end"]);
    let mut result = source.to_string();
    result.replace_range(start..end, edit["newText"].as_str().unwrap());
    result
}

#[test]
fn shutdown_accepts_delayed_watcher_registration_response() {
    let temp = temp_test_dir("shutdown-registration-response");
    let root = temp.join("workspace");
    let library = temp.join("library");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&library).unwrap();
    std::fs::write(root.join("main.rua"), "let value = 1;\n").unwrap();
    std::fs::write(library.join("host.ruai"), "").unwrap();

    let mut server = ProtocolServer::start();
    server.initialize_with(
        vec![json!({ "uri": file_uri(&root), "name": "workspace" })],
        json!({ "rua": { "library": [library.to_string_lossy()] } }),
    );
    let registration = server.server_request("client/registerCapability");

    server.send(json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "shutdown",
        "params": null
    }));
    server.respond_ok(&registration);
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "exit",
        "params": null
    }));

    let response = server.response(99);
    assert_eq!(response.get("result"), Some(&Value::Null));
    server.stdin.take();
    let status = server.child.wait().expect("wait for rua-lsp");
    assert!(status.success(), "rua-lsp exited with {status}");
    std::fs::remove_dir_all(temp).unwrap();
}

fn workspace_symbols(server: &mut ProtocolServer, id: i64, query: &str) -> Value {
    server.send(json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "workspace/symbol",
        "params": { "query": query }
    }));
    server.response(id)["result"].clone()
}

#[test]
fn stdio_src_main_uses_src_as_module_root_for_associated_function_navigation() {
    let temp = temp_test_dir("src-module-root");
    let root = temp.join("workspace");
    let source_root = root.join("src");
    let domain = source_root.join("domain");
    std::fs::create_dir_all(&domain).unwrap();

    let main_source = concat!(
        "use domain::order::OrderRequest;\n",
        "let requests = vec![OrderRequest::new(\"book-001\", 2, 10)];\n",
    );
    let order_source = concat!(
        "pub struct OrderRequest { pub sku: String, pub quantity: i64 }\n",
        "impl OrderRequest {\n",
        "    /// Construct an order request.\n",
        "    pub fn new(sku: String, quantity: i64, discount: i64) -> OrderRequest {\n",
        "        OrderRequest { sku: sku, quantity: quantity }\n",
        "    }\n",
        "}\n",
    );
    let main = source_root.join("main.rua");
    let order = domain.join("order.rua");
    std::fs::write(&main, main_source).unwrap();
    std::fs::write(&order, order_source).unwrap();
    let main_uri = file_uri(&main);
    let order_uri = file_uri(&order);

    let mut server = ProtocolServer::start();
    server.initialize_with(
        vec![json!({ "uri": file_uri(&root), "name": "workspace" })],
        Value::Null,
    );

    let mut indexed = false;
    for request_id in 200..220 {
        let symbols = workspace_symbols(&mut server, request_id, "OrderRequest");
        if symbols
            .as_array()
            .is_some_and(|symbols| !symbols.is_empty())
        {
            indexed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(indexed, "workspace scan did not index OrderRequest");

    server.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": main_uri,
                "languageId": "rua",
                "version": 1,
                "text": main_source
            }
        }
    }));
    server.send(json!({
        "jsonrpc": "2.0",
        "id": 220,
        "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": main_uri },
            "position": position_of(main_source, "new")
        }
    }));
    let hover = server.response(220);
    assert!(hover.to_string().contains("fn new("), "{hover}");

    server.send(json!({
        "jsonrpc": "2.0",
        "id": 221,
        "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": main_uri },
            "position": position_of(main_source, "new")
        }
    }));
    let definition = server.response(221);
    assert_eq!(definition["result"]["uri"], order_uri, "{definition}");

    server.shutdown();
    std::fs::remove_dir_all(temp).unwrap();
}

impl Drop for ProtocolServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn large_project() -> String {
    let mut source = String::from("fn target() {}\n");
    for index in 0..20 {
        source.push_str(&format!("fn caller_{index}() {{ target(); }}\n"));
    }
    source
}

#[test]
fn stdio_inlay_hints_return_inferred_binding_types() {
    let uri = "file:///workspace/inlay-hints.rua";
    let source = "struct Product {} fn first_available() -> Option<Product> { Option::None } fn scores() -> HashMap<String, i64> { #{} } fn main() { let inferred = 42; let explicit: bool = true; let featured = first_available(); let scores = scores(); }\n";
    let mut server = ProtocolServer::start();
    server.initialize();
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "rua",
                "version": 1,
                "text": source
            }
        }
    }));
    server.send(json!({
        "jsonrpc": "2.0",
        "id": 19,
        "method": "textDocument/inlayHint",
        "params": {
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 1, "character": 0 }
            }
        }
    }));
    let hints = server.response(19)["result"].clone();
    let labels = hints
        .as_array()
        .expect("inlay hint result array")
        .iter()
        .map(|hint| inlay_hint_label_text(&hint["label"]))
        .collect::<Vec<_>>();
    assert!(labels.iter().any(|label| label == ": i64"), "{hints}");
    assert!(!labels.iter().any(|label| label == ": bool"), "{hints}");
    assert!(
        labels.iter().any(|label| label == ": Option<Product>"),
        "{hints}"
    );

    let featured = hints
        .as_array()
        .unwrap()
        .iter()
        .find(|hint| inlay_hint_label_text(&hint["label"]) == ": Option<Product>")
        .expect("featured inlay hint");
    let parts = featured["label"].as_array().expect("composite type label");
    for name in ["Option", "Product"] {
        let part = parts
            .iter()
            .find(|part| part["value"] == name)
            .unwrap_or_else(|| panic!("missing {name} label part: {featured}"));
        assert!(part.get("location").is_none(), "{part}");
        assert!(part.get("tooltip").is_some(), "{part}");
        assert_eq!(part["command"]["command"], "rua.openLocation", "{part}");
    }
    let option = parts.iter().find(|part| part["value"] == "Option").unwrap();
    assert!(
        option["tooltip"]["value"]
            .as_str()
            .is_some_and(|value| value.contains("std::option::Option")),
        "{option}"
    );
    let product = parts
        .iter()
        .find(|part| part["value"] == "Product")
        .unwrap();
    assert!(
        product["tooltip"]["value"]
            .as_str()
            .is_some_and(|value| value.contains("struct Product")),
        "{product}"
    );

    let scores = hints
        .as_array()
        .unwrap()
        .iter()
        .find(|hint| inlay_hint_label_text(&hint["label"]) == ": HashMap<String, i64>")
        .expect("scores inlay hint");
    let parts = scores["label"].as_array().expect("composite map label");
    for name in ["HashMap", "String", "i64"] {
        let part = parts
            .iter()
            .find(|part| part["value"] == name)
            .unwrap_or_else(|| panic!("missing {name} label part: {scores}"));
        assert!(part.get("tooltip").is_some(), "{part}");
    }
    for name in ["HashMap", "String"] {
        let part = parts.iter().find(|part| part["value"] == name).unwrap();
        assert_eq!(part["command"]["command"], "rua.openLocation", "{part}");
    }
    let i64_part = parts.iter().find(|part| part["value"] == "i64").unwrap();
    assert!(i64_part.get("command").is_none(), "{i64_part}");
    server.shutdown();
}

fn inlay_hint_label_text(label: &Value) -> String {
    if let Some(label) = label.as_str() {
        return label.to_string();
    }
    label
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["value"].as_str())
        .collect()
}

#[test]
fn stdio_completion_inserts_builtin_generic_type_snippets() {
    let uri = "file:///workspace/builtin-types.rua";
    let source = "fn use_values(option: Opt, result: Res) {}\n";
    let mut server = ProtocolServer::start();
    server.initialize();
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "rua",
                "version": 1,
                "text": source
            }
        }
    }));

    for (id, position, label, snippet) in [
        (20, position_of(source, ","), "Option", "Option<${1:T}>$0"),
        (
            21,
            position_of(source, ")"),
            "Result",
            "Result<${1:T}, ${2:E}>$0",
        ),
    ] {
        server.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": position
            }
        }));
        let response = server.response(id);
        let items = response["result"]
            .as_array()
            .unwrap_or_else(|| panic!("completion result array: {response}"));
        let item = items
            .iter()
            .find(|item| item["label"] == label)
            .unwrap_or_else(|| panic!("missing {label} completion: {response}"));
        assert_eq!(item["insertTextFormat"], 2, "{item}");
        assert_eq!(item["textEdit"]["newText"], snippet, "{item}");
    }
    server.shutdown();
}

#[test]
fn stdio_completion_returns_top_level_chunk_variables_while_typing() {
    let uri = "file:///workspace/chunk-variable-completion.rua";
    let source = concat!(
        "let processed = 1;\n",
        "let featured = processed + 1;\n",
        "println!(\"{}\", fea);\n",
    );
    let mut server = ProtocolServer::start();
    server.initialize();
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "rua",
                "version": 1,
                "text": source
            }
        }
    }));
    server.send(json!({
        "jsonrpc": "2.0",
        "id": 22,
        "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": uri },
            "position": position_after(source, "println!(\"{}\", fea")
        }
    }));

    let response = server.response(22);
    let items = response["result"]
        .as_array()
        .unwrap_or_else(|| panic!("completion result array: {response}"));
    let featured = items
        .iter()
        .find(|item| item["label"] == "featured")
        .unwrap_or_else(|| panic!("missing featured variable: {response}"));
    assert_eq!(featured["kind"], 6, "{featured}");
    assert_eq!(featured["textEdit"]["newText"], "featured", "{featured}");
    server.shutdown();
}

#[test]
fn stdio_code_actions_emit_semantically_valid_edits() {
    let uri = "file:///workspace/code-actions.rua";
    let match_source = concat!(
        "enum Shape { Dot, Pair(i64, i64), Rect { x: i64 } }\n",
        "let shape = Shape::Dot;\n",
        "match shape {\n",
        "    Shape::Dot => {}\n",
        "}\n",
    );
    let mut server = ProtocolServer::start();
    server.initialize();
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "rua",
                "version": 1,
                "text": match_source
            }
        }
    }));
    server.send(json!({
        "jsonrpc": "2.0",
        "id": 20,
        "method": "textDocument/codeAction",
        "params": {
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": 2, "character": 0 },
                "end": { "line": 4, "character": 1 }
            },
            "context": { "diagnostics": [] }
        }
    }));
    let actions = server.response(20)["result"].clone();
    let fill = actions
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["title"] == "Fill match arms (2 missing)")
        .unwrap_or_else(|| panic!("missing fill action: {actions}"));
    let fill_edit = &fill["edit"]["changes"][uri][0];
    let fill_text = fill_edit["newText"].as_str().unwrap();
    assert!(fill_text.contains("Pair(_, _) => todo!(),"), "{fill_text}");
    assert!(fill_text.contains("Rect { .. } => todo!(),"), "{fill_text}");
    assert!(!fill_text.contains("Dot =>"), "{fill_text}");
    let filled = apply_single_edit(match_source, fill_edit);
    let filled_parse = rua_syntax::parse(&filled);
    assert!(
        filled_parse.errors().is_empty(),
        "invalid fill edit: {:?}\n{filled}",
        filled_parse.errors()
    );

    let mut_source = concat!(
        "fn update() {\n",
        "    let x = 1;\n",
        "    let y = 2;\n",
        "    x = y;\n",
        "}\n",
        "update();\n",
    );
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{ "text": mut_source }]
        }
    }));
    let published = server.matching_message(|message| {
        message["method"] == "textDocument/publishDiagnostics"
            && message["params"]["uri"] == uri
            && message["params"]["version"] == 2
            && message["params"]["diagnostics"]
                .as_array()
                .is_some_and(|diagnostics| {
                    diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic["code"] == "E0212")
                })
    });
    let immutable_diagnostic = published["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|diagnostic| diagnostic["code"] == "E0212")
        .unwrap()
        .clone();
    let assignment_range = immutable_diagnostic["range"].clone();
    server.send(json!({
        "jsonrpc": "2.0",
        "id": 21,
        "method": "textDocument/codeAction",
        "params": {
            "textDocument": { "uri": uri },
            "range": assignment_range.clone(),
            "context": {
                "diagnostics": [immutable_diagnostic]
            }
        }
    }));
    let actions = server.response(21)["result"].clone();
    let add_mut = actions
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["title"] == "Add `mut` to variable")
        .unwrap_or_else(|| panic!("missing add-mut action: {actions}"));
    let mut_edit = &add_mut["edit"]["changes"][uri][0];
    assert_eq!(
        mut_edit["range"]["start"],
        json!({ "line": 1, "character": 8 })
    );
    assert_eq!(mut_edit["newText"], "mut ");
    let mutated = apply_single_edit(mut_source, mut_edit);
    assert!(mutated.contains("let mut x = 1;"), "{mutated}");
    assert!(
        rua_syntax::parse(&mutated).errors().is_empty(),
        "invalid add-mut edit:\n{mutated}"
    );

    let unused_source = "fn run() { let value = 1; }\nrun();\n";
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": 3 },
            "contentChanges": [{ "text": unused_source }]
        }
    }));
    let unused_range = json!({
        "start": { "line": 0, "character": 15 },
        "end": { "line": 0, "character": 20 }
    });
    server.send(json!({
        "jsonrpc": "2.0",
        "id": 22,
        "method": "textDocument/codeAction",
        "params": {
            "textDocument": { "uri": uri },
            "range": unused_range.clone(),
            "context": {
                "diagnostics": [{
                    "range": unused_range,
                    "message": "unused variable `value`",
                    "code": "W0300",
                    "source": "rua"
                }]
            }
        }
    }));
    let actions = server.response(22)["result"].clone();
    let suppress_unused = actions
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["title"] == "Rename to `_value` (suppress warning)")
        .unwrap_or_else(|| panic!("missing unused-variable action: {actions}"));
    let unused_edit = &suppress_unused["edit"]["changes"][uri][0];
    let suppressed = apply_single_edit(unused_source, unused_edit);
    assert!(suppressed.contains("let _value = 1;"), "{suppressed}");

    let trailing_source = concat!(
        "struct Point { x: i64 }\n",
        "let point = Point { x: 1, };\n",
    );
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": 4 },
            "contentChanges": [{ "text": trailing_source }]
        }
    }));
    let comma = json!({ "line": 1, "character": 24 });
    server.send(json!({
        "jsonrpc": "2.0",
        "id": 23,
        "method": "textDocument/codeAction",
        "params": {
            "textDocument": { "uri": uri },
            "range": { "start": comma.clone(), "end": comma },
            "context": { "diagnostics": [] }
        }
    }));
    let actions = server.response(23)["result"].clone();
    let remove_comma = actions
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["title"] == "Remove trailing comma")
        .unwrap_or_else(|| panic!("missing trailing-comma action: {actions}"));
    let comma_edit = &remove_comma["edit"]["changes"][uri][0];
    let without_comma = apply_single_edit(trailing_source, comma_edit);
    assert!(without_comma.contains("Point { x: 1 }"), "{without_comma}");
    assert!(
        rua_syntax::parse(&without_comma).errors().is_empty(),
        "invalid trailing-comma edit:\n{without_comma}"
    );

    server.shutdown();
}

#[test]
fn stdio_hierarchy_round_trips_identity_and_exact_call_ranges() {
    let uri = "file:///workspace/hierarchy.rua";
    let source = concat!(
        "trait Mark { fn mark(&self); }\n",
        "struct Thing {}\n",
        "impl Mark for Thing { fn mark(&self) {} }\n",
        "fn helper() {}\n",
        "fn caller() { helper(); helper(); }\n",
    );
    let mut server = ProtocolServer::start();
    server.initialize();
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "rua",
                "version": 1,
                "text": source
            }
        }
    }));

    server.send(json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "textDocument/prepareCallHierarchy",
        "params": {
            "textDocument": { "uri": uri },
            "position": position_of(source, "caller")
        }
    }));
    let prepared = server.response(10)["result"][0].clone();
    assert!(prepared["data"]["target"].is_number(), "{prepared}");

    server.send(json!({
        "jsonrpc": "2.0",
        "id": 11,
        "method": "callHierarchy/outgoingCalls",
        "params": { "item": prepared }
    }));
    let outgoing = server.response(11)["result"].clone();
    assert_eq!(outgoing.as_array().map(Vec::len), Some(1), "{outgoing}");
    assert_eq!(outgoing[0]["to"]["name"], "helper", "{outgoing}");
    assert_eq!(
        outgoing[0]["fromRanges"].as_array().map(Vec::len),
        Some(2),
        "{outgoing}"
    );
    assert!(outgoing[0]["to"]["data"]["target"].is_number());

    server.send(json!({
        "jsonrpc": "2.0",
        "id": 12,
        "method": "textDocument/prepareTypeHierarchy",
        "params": {
            "textDocument": { "uri": uri },
            "position": position_of(source, "Thing")
        }
    }));
    let prepared_type = server.response(12)["result"][0].clone();
    assert!(prepared_type["data"]["target"].is_number());
    server.send(json!({
        "jsonrpc": "2.0",
        "id": 13,
        "method": "typeHierarchy/supertypes",
        "params": { "item": prepared_type }
    }));
    let supertypes = server.response(13)["result"].clone();
    assert_eq!(supertypes[0]["name"], "Mark", "{supertypes}");
    assert!(supertypes[0]["data"]["target"].is_number());

    server.shutdown();
}

#[test]
fn stdio_lifecycle_cancels_queries_and_rejects_stale_results() {
    let uri = "file:///workspace/main.rua";
    let mut server = ProtocolServer::start();
    server.initialize();
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": "rua",
                "version": 1,
                "text": large_project()
            }
        }
    }));

    server.send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "textDocument/references",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 4 },
            "context": { "includeDeclaration": true }
        }
    }));
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "$/cancelRequest",
        "params": { "id": 2 }
    }));
    let cancelled = server.response(2);
    assert_eq!(cancelled["error"]["code"], -32800, "{cancelled}");

    server.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "textDocument/references",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 4 },
            "context": { "includeDeclaration": true }
        }
    }));
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{
                "text": "/// Current target.\nfn target() {}\nfn caller() { target(); }\n"
            }]
        }
    }));
    let stale = server.response(3);
    assert_eq!(stale["error"]["code"], -32801, "{stale}");

    server.send(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": 1, "character": 4 }
        }
    }));
    let hover = server.response(4);
    assert!(hover.get("result").is_some(), "{hover}");

    server.send(json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "workspace/symbol",
        "params": { "query": "target" }
    }));
    let symbols = server.response(5);
    assert_eq!(symbols["result"][0]["name"], "target", "{symbols}");

    server.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didSave",
        "params": { "textDocument": { "uri": uri } }
    }));
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didClose",
        "params": { "textDocument": { "uri": uri } }
    }));
    server.shutdown();
}

#[test]
fn stdio_multi_root_library_watchers_and_overlay_lifecycle() {
    let temp = temp_test_dir("workspace-lifecycle");
    let first_root = temp.join("first");
    let second_root = temp.join("second");
    let library_root = temp.join("library");
    for directory in [&first_root, &second_root, &library_root] {
        std::fs::create_dir_all(directory).unwrap();
    }

    let first_source = concat!(
        "fn call_api() { api::first_api(); }\n",
        "fn first_only() {}\n",
        "fn disk_only() {}\n",
    );
    let second_source = "fn second_only() {}\n";
    let first_main = first_root.join("main.rua");
    let second_main = second_root.join("main.rua");
    std::fs::write(&first_main, first_source).unwrap();
    std::fs::write(&second_main, second_source).unwrap();

    let first_main_uri = file_uri(&first_main);
    let second_main_uri = file_uri(&second_main);
    let mut server = ProtocolServer::start();
    server.initialize_with(
        vec![
            json!({ "uri": file_uri(&first_root), "name": "first" }),
            json!({ "uri": file_uri(&second_root), "name": "second" }),
        ],
        json!({
            "rua": { "library": [library_root.to_string_lossy()] }
        }),
    );

    let registration = server.server_request("client/registerCapability");
    assert_eq!(
        registration["params"]["registrations"][0]["method"], "workspace/didChangeWatchedFiles",
        "{registration}"
    );
    server.respond_ok(&registration);

    let first_symbols = workspace_symbols(&mut server, 10, "first_only");
    assert_eq!(
        first_symbols.as_array().map(Vec::len),
        Some(1),
        "{first_symbols}"
    );
    assert_eq!(first_symbols[0]["location"]["uri"], first_main_uri);
    let second_symbols = workspace_symbols(&mut server, 11, "second_only");
    assert_eq!(
        second_symbols.as_array().map(Vec::len),
        Some(1),
        "{second_symbols}"
    );
    assert_eq!(second_symbols[0]["location"]["uri"], second_main_uri);

    let declaration = library_root.join("api.ruai");
    std::fs::write(
        &declaration,
        "/// Initial host documentation.\npub fn first_api();\n",
    )
    .unwrap();
    let declaration_uri = file_uri(&declaration);
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "workspace/didChangeWatchedFiles",
        "params": {
            "changes": [{ "uri": declaration_uri, "type": 1 }]
        }
    }));

    server.send(json!({
        "jsonrpc": "2.0",
        "id": 12,
        "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": first_main_uri },
            "position": position_of(first_source, "first_api")
        }
    }));
    let definition = server.response(12);
    assert_eq!(definition["result"]["uri"], declaration_uri, "{definition}");

    server.send(json!({
        "jsonrpc": "2.0",
        "id": 13,
        "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": first_main_uri },
            "position": position_of(first_source, "first_api")
        }
    }));
    let initial_hover = server.response(13);
    assert!(
        initial_hover
            .to_string()
            .contains("Initial host documentation."),
        "{initial_hover}"
    );

    std::fs::write(
        &declaration,
        "/// Updated host documentation.\npub fn first_api();\n",
    )
    .unwrap();
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "workspace/didChangeWatchedFiles",
        "params": {
            "changes": [{ "uri": declaration_uri, "type": 2 }]
        }
    }));
    server.send(json!({
        "jsonrpc": "2.0",
        "id": 14,
        "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": first_main_uri },
            "position": position_of(first_source, "first_api")
        }
    }));
    let updated_hover = server.response(14);
    assert!(
        updated_hover
            .to_string()
            .contains("Updated host documentation."),
        "{updated_hover}"
    );

    let overlay_source = "fn overlay_only() {}\n";
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": first_main_uri,
                "languageId": "rua",
                "version": 1,
                "text": overlay_source
            }
        }
    }));
    let overlay_symbols = workspace_symbols(&mut server, 15, "overlay_only");
    assert_eq!(
        overlay_symbols.as_array().map(Vec::len),
        Some(1),
        "{overlay_symbols}"
    );
    assert!(
        workspace_symbols(&mut server, 16, "disk_only")
            .as_array()
            .is_some_and(Vec::is_empty)
    );

    server.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didClose",
        "params": { "textDocument": { "uri": first_main_uri } }
    }));
    let restored_symbols = workspace_symbols(&mut server, 17, "disk_only");
    assert_eq!(
        restored_symbols.as_array().map(Vec::len),
        Some(1),
        "{restored_symbols}"
    );
    assert!(
        workspace_symbols(&mut server, 18, "overlay_only")
            .as_array()
            .is_some_and(Vec::is_empty)
    );

    std::fs::remove_file(&declaration).unwrap();
    server.send(json!({
        "jsonrpc": "2.0",
        "method": "workspace/didChangeWatchedFiles",
        "params": {
            "changes": [{ "uri": declaration_uri, "type": 3 }]
        }
    }));
    server.send(json!({
        "jsonrpc": "2.0",
        "id": 19,
        "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": first_main_uri },
            "position": position_of(first_source, "first_api")
        }
    }));
    let deleted_definition = server.response(19);
    assert_eq!(
        deleted_definition["result"],
        Value::Null,
        "{deleted_definition}"
    );

    server.send(json!({
        "jsonrpc": "2.0",
        "method": "workspace/didChangeConfiguration",
        "params": { "settings": { "rua": { "library": [] } } }
    }));
    let unregistration = server.server_request("client/unregisterCapability");
    assert_eq!(
        unregistration["params"]["unregisterations"][0]["method"],
        "workspace/didChangeWatchedFiles",
        "{unregistration}"
    );
    server.respond_ok(&unregistration);
    assert!(
        workspace_symbols(&mut server, 20, "first_api")
            .as_array()
            .is_some_and(Vec::is_empty)
    );

    server.shutdown();
    std::fs::remove_dir_all(temp).unwrap();
}
