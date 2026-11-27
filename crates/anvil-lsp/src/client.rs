//! Stateful LSP client managing a single language server child process.

use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error::{LspError, Result};
use crate::protocol::{read_message, write_message};
use crate::types::{
    severity_name, symbol_kind_name, DiagnosticItem, HoverInfo, LanguageKind, LocationItem, SymbolItem,
};
use crate::uri::{normalize_uri, path_to_uri, uri_to_path};

/// Specification for launching a language server process.
#[derive(Debug, Clone)]
pub struct ServerCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, std::ffi::OsString)>,
}

struct OpenDoc {
    version: i64,
    content: String,
}

#[derive(Default)]
struct DiagStore {
    by_uri: HashMap<String, (u64, Vec<DiagnosticItem>)>,
    seq: u64,
}

struct SharedState {
    pending: Mutex<HashMap<i64, Sender<Result<Value>>>>,
    diagnostics: Mutex<DiagStore>,
    diag_cv: Condvar,
}

/// Live connection to one language server (`rust-analyzer` or `clangd`).
pub struct LspClient {
    kind: LanguageKind,
    workspace_root: PathBuf,
    stdin: Arc<Mutex<ChildStdin>>,
    child: Mutex<Child>,
    next_id: AtomicI64,
    shared: Arc<SharedState>,
    open_docs: Mutex<HashMap<String, OpenDoc>>,
    supports_pull_diagnostics: bool,
    last_used: Mutex<Instant>,
    _reader: Option<JoinHandle<()>>,
    _stderr: Option<JoinHandle<()>>,
}

impl LspClient {
    /// Start `server_cmd` for `workspace_root` and complete the LSP `initialize` handshake.
    pub fn start(
        kind: LanguageKind,
        workspace_root: PathBuf,
        server_cmd: &ServerCommand,
        init_timeout: Duration,
    ) -> Result<Self> {
        let mut cmd = Command::new(&server_cmd.program);
        cmd.args(&server_cmd.args)
            .current_dir(&workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &server_cmd.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|source| LspError::Spawn {
            server: server_cmd.program.display().to_string(),
            source,
        })?;

        let stdin = Arc::new(Mutex::new(child.stdin.take().ok_or(LspError::Disconnected)?));
        let stdout = child.stdout.take().ok_or(LspError::Disconnected)?;
        let stderr = child.stderr.take();

        let shared = Arc::new(SharedState {
            pending: Mutex::new(HashMap::new()),
            diagnostics: Mutex::new(DiagStore::default()),
            diag_cv: Condvar::new(),
        });

        let reader_shared = Arc::clone(&shared);
        let reader_stdin = Arc::clone(&stdin);
        let reader = thread::spawn(move || {
            reader_loop(stdout, reader_shared, reader_stdin);
        });

        let stderr_handle = stderr.map(|mut err| {
            thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = err.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                }
            })
        });

        let mut client = Self {
            kind,
            workspace_root: workspace_root.clone(),
            stdin,
            child: Mutex::new(child),
            next_id: AtomicI64::new(1),
            shared,
            open_docs: Mutex::new(HashMap::new()),
            supports_pull_diagnostics: false,
            last_used: Mutex::new(Instant::now()),
            _reader: Some(reader),
            _stderr: stderr_handle,
        };

        let root_uri = path_to_uri(&workspace_root)?;
        let init_params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "workspaceFolders": [{
                "uri": root_uri,
                "name": workspace_root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "workspace".into()),
            }],
            "capabilities": {
                "textDocument": {
                    "hover": { "contentFormat": ["markdown", "plaintext"] },
                    "definition": { "linkSupport": true },
                    "references": {},
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                    "publishDiagnostics": { "relatedInformation": true },
                    "diagnostic": {}
                },
                "workspace": {
                    "symbol": {},
                    "workspaceFolders": true
                }
            },
            "clientInfo": {
                "name": "anvil",
                "version": anvil_core::ANVIL_VERSION
            }
        });

        let init_result = client.request("initialize", init_params, init_timeout)?;
        client.supports_pull_diagnostics = init_result
            .get("capabilities")
            .and_then(|c| c.get("diagnosticProvider"))
            .is_some_and(|v| !v.is_null() && v != &Value::Bool(false));

        client.notify("initialized", json!({}))?;
        Ok(client)
    }

    pub fn kind(&self) -> LanguageKind {
        self.kind
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn last_used(&self) -> Instant {
        *self.last_used.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_alive(&self) -> bool {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        matches!(child.try_wait(), Ok(None))
    }

    fn touch(&self) {
        if let Ok(mut ts) = self.last_used.lock() {
            *ts = Instant::now();
        }
    }

    /// Ensure `file` is open and up to date with its on-disk content. Returns `(uri, changed)`.
    pub fn sync_document(&self, file: &Path) -> Result<(String, bool)> {
        self.touch();
        let uri = path_to_uri(file)?;
        let content = std::fs::read_to_string(file)?;
        let mut docs = self.open_docs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(doc) = docs.get_mut(&uri) {
            if doc.content == content {
                return Ok((uri, false));
            }
            doc.version += 1;
            doc.content = content.clone();
            let version = doc.version;
            drop(docs);
            self.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": uri, "version": version },
                    "contentChanges": [{ "text": content }],
                }),
            )?;
            return Ok((uri, true));
        }

        let language_id = self.kind.lsp_language_id(file);
        docs.insert(
            uri.clone(),
            OpenDoc {
                version: 1,
                content: content.clone(),
            },
        );
        drop(docs);
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": content,
                }
            }),
        )?;
        Ok((uri, true))
    }

    /// Fetch diagnostics for `file`, using pull diagnostics when supported or waiting for `publishDiagnostics`.
    pub fn diagnostics(&self, file: &Path, wait: Duration, timeout: Duration) -> Result<Vec<DiagnosticItem>> {
        let key = normalize_uri(&path_to_uri(file)?);
        let prev_seq = {
            let store = self.shared.diagnostics.lock().unwrap_or_else(|e| e.into_inner());
            store.by_uri.get(&key).map(|(s, _)| *s).unwrap_or(0)
        };
        let (uri, changed) = self.sync_document(file)?;

        if self.supports_pull_diagnostics {
            if let Ok(val) = self.request(
                "textDocument/diagnostic",
                json!({ "textDocument": { "uri": uri } }),
                timeout,
            ) {
                if let Some(items) = val.get("items").and_then(Value::as_array) {
                    return Ok(items.iter().filter_map(parse_diagnostic).collect());
                }
            }
        }

        // Wait up to `wait` for a fresh publishDiagnostics notification.
        let deadline = Instant::now() + wait;
        let mut store = self.shared.diagnostics.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some((seq, items)) = store.by_uri.get(&key) {
                if !changed || *seq > prev_seq {
                    return Ok(items.clone());
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(store
                    .by_uri
                    .get(&key)
                    .map(|(_, items)| items.clone())
                    .unwrap_or_default());
            }
            let (guard, _) = self
                .shared
                .diag_cv
                .wait_timeout(store, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
            store = guard;
        }
    }

    /// Hover information at 1-based `(line, col)`.
    pub fn hover(&self, file: &Path, line: u32, col: u32, timeout: Duration) -> Result<Option<HoverInfo>> {
        let (uri, _) = self.sync_document(file)?;
        let val = self.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line.saturating_sub(1), "character": col.saturating_sub(1) }
            }),
            timeout,
        )?;
        if val.is_null() {
            return Ok(None);
        }
        let contents = extract_hover_contents(val.get("contents"));
        if contents.is_empty() {
            return Ok(None);
        }
        let range = val.get("range");
        let start_line = range
            .and_then(|r| r.pointer("/start/line"))
            .and_then(Value::as_u64)
            .map(|n| n as u32 + 1);
        let start_col = range
            .and_then(|r| r.pointer("/start/character"))
            .and_then(Value::as_u64)
            .map(|n| n as u32 + 1);
        Ok(Some(HoverInfo {
            contents,
            line: start_line,
            col: start_col,
        }))
    }

    /// Go-to-definition at 1-based `(line, col)`.
    pub fn definition(&self, file: &Path, line: u32, col: u32, timeout: Duration) -> Result<Vec<LocationItem>> {
        let (uri, _) = self.sync_document(file)?;
        let val = self.request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line.saturating_sub(1), "character": col.saturating_sub(1) }
            }),
            timeout,
        )?;
        Ok(parse_locations(&val))
    }

    /// Find references at 1-based `(line, col)`.
    pub fn references(
        &self,
        file: &Path,
        line: u32,
        col: u32,
        include_declaration: bool,
        timeout: Duration,
    ) -> Result<Vec<LocationItem>> {
        let (uri, _) = self.sync_document(file)?;
        let val = self.request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line.saturating_sub(1), "character": col.saturating_sub(1) },
                "context": { "includeDeclaration": include_declaration }
            }),
            timeout,
        )?;
        Ok(parse_locations(&val))
    }

    /// List symbols in a single document.
    pub fn document_symbols(&self, file: &Path, timeout: Duration) -> Result<Vec<SymbolItem>> {
        let (uri, _) = self.sync_document(file)?;
        let val = self.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
            timeout,
        )?;
        let mut out = Vec::new();
        if let Some(arr) = val.as_array() {
            for item in arr {
                collect_doc_symbol(item, file, None, &mut out);
            }
        }
        Ok(out)
    }

    /// Search symbols across the workspace.
    pub fn workspace_symbols(&self, query: &str, timeout: Duration) -> Result<Vec<SymbolItem>> {
        self.touch();
        let val = self.request("workspace/symbol", json!({ "query": query }), timeout)?;
        let mut out = Vec::new();
        if let Some(arr) = val.as_array() {
            for item in arr {
                if let Some(sym) = parse_symbol_information(item) {
                    out.push(sym);
                }
            }
        }
        Ok(out)
    }

    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx): (Sender<Result<Value>>, Receiver<Result<Value>>) = mpsc::channel();
        {
            let mut pending = self.shared.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.insert(id, tx);
        }
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(err) = self.send_raw(&msg) {
            let mut pending = self.shared.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.remove(&id);
            return Err(err);
        }

        match rx.recv_timeout(timeout) {
            Ok(Ok(val)) => Ok(val),
            Ok(Err(LspError::Rpc { code, message, .. })) => Err(LspError::Rpc {
                method: method.to_string(),
                code,
                message,
            }),
            Ok(Err(e)) => Err(e),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let mut pending = self.shared.pending.lock().unwrap_or_else(|e| e.into_inner());
                pending.remove(&id);
                Err(LspError::Timeout {
                    method: method.to_string(),
                    timeout_ms: timeout.as_millis() as u64,
                })
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(LspError::Disconnected),
        }
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.send_raw(&msg)
    }

    fn send_raw(&self, msg: &Value) -> Result<()> {
        let mut stdin = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        write_message(&mut *stdin, msg)
    }

    /// Gracefully shut down the language server.
    pub fn shutdown(&self) {
        let _ = self.request("shutdown", Value::Null, Duration::from_millis(800));
        let _ = self.notify("exit", Value::Null);
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let deadline = Instant::now() + Duration::from_millis(800);
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn reader_loop<R: Read>(stdout: R, shared: Arc<SharedState>, stdin: Arc<Mutex<ChildStdin>>) {
    let mut reader = BufReader::new(stdout);
    while let Ok(Some(msg)) = read_message(&mut reader) {
        let has_id = msg.get("id").is_some();
        let method = msg.get("method").and_then(Value::as_str);

        match (has_id, method) {
            // Response to a client request.
            (true, None) => {
                if let Some(id) = msg.get("id").and_then(Value::as_i64) {
                    let tx = {
                        let mut pending = shared.pending.lock().unwrap_or_else(|e| e.into_inner());
                        pending.remove(&id)
                    };
                    if let Some(tx) = tx {
                        let res = if let Some(err) = msg.get("error") {
                            let code = err.get("code").and_then(Value::as_i64).unwrap_or(-32603);
                            let message = err
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("LSP error")
                                .to_string();
                            Err(LspError::Rpc {
                                method: String::new(),
                                code,
                                message,
                            })
                        } else {
                            Ok(msg.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = tx.send(res);
                    }
                }
            }
            // Server-to-client request: reply immediately so servers never deadlock.
            (true, Some(req_method)) => {
                let id = msg.get("id").cloned().unwrap_or(Value::Null);
                let result = if req_method == "workspace/configuration" {
                    let count = msg
                        .pointer("/params/items")
                        .and_then(Value::as_array)
                        .map_or(1, Vec::len);
                    Value::Array(vec![json!({}); count])
                } else {
                    Value::Null
                };
                let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
                if let Ok(mut guard) = stdin.lock() {
                    let _ = write_message(&mut *guard, &reply);
                }
            }
            // Server notification.
            (false, Some("textDocument/publishDiagnostics")) => {
                if let Some(params) = msg.get("params") {
                    if let Some(uri) = params.get("uri").and_then(Value::as_str) {
                        let items: Vec<DiagnosticItem> = params
                            .get("diagnostics")
                            .and_then(Value::as_array)
                            .map(|arr| arr.iter().filter_map(parse_diagnostic).collect())
                            .unwrap_or_default();
                        let key = normalize_uri(uri);
                        let mut store = shared.diagnostics.lock().unwrap_or_else(|e| e.into_inner());
                        store.seq += 1;
                        let seq = store.seq;
                        store.by_uri.insert(key, (seq, items));
                        shared.diag_cv.notify_all();
                    }
                }
            }
            _ => {}
        }
    }

    // Wake up any waiters on disconnect.
    let waiters: Vec<_> = {
        let mut pending = shared.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.drain().map(|(_, tx)| tx).collect()
    };
    for tx in waiters {
        let _ = tx.send(Err(LspError::Disconnected));
    }
}

fn parse_diagnostic(val: &Value) -> Option<DiagnosticItem> {
    let message = val.get("message")?.as_str()?.to_string();
    let range = val.get("range")?;
    let line = range.pointer("/start/line").and_then(Value::as_u64).unwrap_or(0) as u32 + 1;
    let col = range.pointer("/start/character").and_then(Value::as_u64).unwrap_or(0) as u32 + 1;
    let end_line = range
        .pointer("/end/line")
        .and_then(Value::as_u64)
        .unwrap_or((line - 1) as u64) as u32
        + 1;
    let end_col = range
        .pointer("/end/character")
        .and_then(Value::as_u64)
        .unwrap_or((col - 1) as u64) as u32
        + 1;
    let severity = severity_name(val.get("severity").and_then(Value::as_u64));
    let code = val.get("code").and_then(|c| match c {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    });
    let source = val.get("source").and_then(Value::as_str).map(str::to_string);
    Some(DiagnosticItem {
        line,
        col,
        end_line,
        end_col,
        severity,
        code,
        source,
        message,
    })
}

fn extract_hover_contents(val: Option<&Value>) -> String {
    let Some(val) = val else {
        return String::new();
    };
    match val {
        Value::String(s) => s.trim().to_string(),
        Value::Object(obj) => obj
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        Value::Array(arr) => arr
            .iter()
            .map(|item| extract_hover_contents(Some(item)))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

fn parse_locations(val: &Value) -> Vec<LocationItem> {
    match val {
        Value::Null => Vec::new(),
        Value::Object(_) => parse_one_location(val).into_iter().collect(),
        Value::Array(arr) => arr.iter().filter_map(parse_one_location).collect(),
        _ => Vec::new(),
    }
}

fn parse_one_location(val: &Value) -> Option<LocationItem> {
    // Support both Location (`uri`, `range`) and LocationLink (`targetUri`, `targetSelectionRange`/`targetRange`).
    let (uri, range) = if let Some(u) = val.get("uri").and_then(Value::as_str) {
        (u, val.get("range")?)
    } else {
        let u = val.get("targetUri").and_then(Value::as_str)?;
        (u, val.get("targetSelectionRange").or_else(|| val.get("targetRange"))?)
    };
    let file = uri_to_path(uri)?;
    let line = range.pointer("/start/line").and_then(Value::as_u64)? as u32 + 1;
    let col = range.pointer("/start/character").and_then(Value::as_u64)? as u32 + 1;
    let end_line = range.pointer("/end/line").and_then(Value::as_u64).unwrap_or(0) as u32 + 1;
    let end_col = range.pointer("/end/character").and_then(Value::as_u64).unwrap_or(0) as u32 + 1;
    Some(LocationItem {
        file,
        line,
        col,
        end_line,
        end_col,
    })
}

fn collect_doc_symbol(val: &Value, file: &Path, container: Option<&str>, out: &mut Vec<SymbolItem>) {
    // SymbolInformation has `location`; DocumentSymbol has `range`/`selectionRange` and optional `children`.
    if val.get("location").is_some() {
        if let Some(sym) = parse_symbol_information(val) {
            out.push(sym);
        }
        return;
    }
    let Some(name) = val.get("name").and_then(Value::as_str) else {
        return;
    };
    let kind = symbol_kind_name(val.get("kind").and_then(Value::as_u64).unwrap_or(0)).to_string();
    let detail = val
        .get("detail")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let range = val.get("selectionRange").or_else(|| val.get("range"));
    let line = range
        .and_then(|r| r.pointer("/start/line"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32
        + 1;
    let col = range
        .and_then(|r| r.pointer("/start/character"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32
        + 1;

    out.push(SymbolItem {
        name: name.to_string(),
        kind,
        detail,
        container: container.map(str::to_string),
        file: Some(file.to_path_buf()),
        line,
        col,
    });

    if let Some(children) = val.get("children").and_then(Value::as_array) {
        for child in children {
            collect_doc_symbol(child, file, Some(name), out);
        }
    }
}

fn parse_symbol_information(val: &Value) -> Option<SymbolItem> {
    let name = val.get("name")?.as_str()?.to_string();
    let kind = symbol_kind_name(val.get("kind").and_then(Value::as_u64).unwrap_or(0)).to_string();
    let container = val
        .get("containerName")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let loc = val.get("location").and_then(parse_one_location);
    Some(SymbolItem {
        name,
        kind,
        detail: None,
        container,
        file: loc.as_ref().map(|l| l.file.clone()),
        line: loc.as_ref().map_or(1, |l| l.line),
        col: loc.as_ref().map_or(1, |l| l.col),
    })
}
