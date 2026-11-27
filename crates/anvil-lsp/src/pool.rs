//! Pool of lazy-started LSP clients keyed by `(LanguageKind, workspace_root)`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anvil_core::InstallLayout;
use anvil_env::dispatch::ToolEnv;
use anvil_env::tools::{find_real_tool, managed_tool};

use crate::client::{LspClient, ServerCommand};
use crate::error::{LspError, Result};
use crate::types::{discover_workspace_root, DiagnosticItem, HoverInfo, LanguageKind, LocationItem, SymbolItem};

const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(600);
const DEFAULT_INIT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_REQ_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_DIAG_WAIT: Duration = Duration::from_millis(1200);
const DEFAULT_RUST_DIAG_WAIT: Duration = Duration::from_millis(10_000);

/// Manages live `rust-analyzer` and `clangd` instances across workspaces.
pub struct LspPool {
    layout: InstallLayout,
    default_workspace: Option<PathBuf>,
    overrides: HashMap<LanguageKind, ServerCommand>,
    clients: Mutex<HashMap<(LanguageKind, PathBuf), Arc<LspClient>>>,
    idle_timeout: Duration,
    init_timeout: Duration,
    req_timeout: Duration,
}

impl LspPool {
    pub fn new(layout: InstallLayout, default_workspace: Option<PathBuf>) -> Self {
        Self {
            layout,
            default_workspace,
            overrides: HashMap::new(),
            clients: Mutex::new(HashMap::new()),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            init_timeout: DEFAULT_INIT_TIMEOUT,
            req_timeout: DEFAULT_REQ_TIMEOUT,
        }
    }

    /// Override the server command for a language (used in tests or custom setups).
    pub fn with_server_override(mut self, kind: LanguageKind, cmd: ServerCommand) -> Self {
        self.overrides.insert(kind, cmd);
        self
    }

    pub fn with_timeouts(mut self, init: Duration, req: Duration, idle: Duration) -> Self {
        self.init_timeout = init;
        self.req_timeout = req;
        self.idle_timeout = idle;
        self
    }

    /// Evict servers that have been idle longer than `idle_timeout` or have exited.
    pub fn evict_stale(&self) {
        let now = Instant::now();
        let stale: Vec<Arc<LspClient>> = {
            let mut map = self.clients.lock().unwrap_or_else(|e| e.into_inner());
            let keys: Vec<_> = map
                .iter()
                .filter(|(_, c)| !c.is_alive() || now.duration_since(c.last_used()) >= self.idle_timeout)
                .map(|(k, _)| k.clone())
                .collect();
            keys.iter().filter_map(|k| map.remove(k)).collect()
        };
        // Shutting a server down can take a while: never do it while holding the map lock.
        for client in stale {
            client.shutdown();
        }
    }

    /// Shut down every active language server cleanly.
    pub fn shutdown_all(&self) {
        let clients: Vec<Arc<LspClient>> = {
            let mut map = self.clients.lock().unwrap_or_else(|e| e.into_inner());
            map.drain().map(|(_, c)| c).collect()
        };
        for client in clients {
            client.shutdown();
        }
    }

    pub fn diagnostics(&self, file: &Path, wait: Option<Duration>) -> Result<Vec<DiagnosticItem>> {
        let abs = resolve_file(file, self.default_workspace.as_deref())?;
        let (kind, root) = self.classify_file(&abs)?;
        let client = self.client_for(kind, &root)?;
        let default_wait = match kind {
            LanguageKind::Rust => DEFAULT_RUST_DIAG_WAIT,
            LanguageKind::Cpp => DEFAULT_DIAG_WAIT,
        };
        client.diagnostics(&abs, wait.unwrap_or(default_wait), self.req_timeout)
    }

    pub fn hover(&self, file: &Path, line: u32, col: u32) -> Result<Option<HoverInfo>> {
        let abs = resolve_file(file, self.default_workspace.as_deref())?;
        let (kind, root) = self.classify_file(&abs)?;
        let client = self.client_for(kind, &root)?;
        client.hover(&abs, line, col, self.req_timeout)
    }

    pub fn definition(&self, file: &Path, line: u32, col: u32) -> Result<Vec<LocationItem>> {
        let abs = resolve_file(file, self.default_workspace.as_deref())?;
        let (kind, root) = self.classify_file(&abs)?;
        let client = self.client_for(kind, &root)?;
        client.definition(&abs, line, col, self.req_timeout)
    }

    pub fn references(&self, file: &Path, line: u32, col: u32, include_declaration: bool) -> Result<Vec<LocationItem>> {
        let abs = resolve_file(file, self.default_workspace.as_deref())?;
        let (kind, root) = self.classify_file(&abs)?;
        let client = self.client_for(kind, &root)?;
        client.references(&abs, line, col, include_declaration, self.req_timeout)
    }

    pub fn document_symbols(&self, file: &Path) -> Result<Vec<SymbolItem>> {
        let abs = resolve_file(file, self.default_workspace.as_deref())?;
        let (kind, root) = self.classify_file(&abs)?;
        let client = self.client_for(kind, &root)?;
        client.document_symbols(&abs, self.req_timeout)
    }

    pub fn workspace_symbols(
        &self,
        query: &str,
        workspace: Option<&Path>,
        language_hint: Option<LanguageKind>,
    ) -> Result<Vec<SymbolItem>> {
        let root = workspace
            .map(Path::to_path_buf)
            .or_else(|| self.default_workspace.clone())
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| LspError::InvalidPath("cannot determine workspace directory".into()))?;
        let root = std::path::absolute(&root).unwrap_or(root);
        let kind = language_hint
            .or_else(|| detect_workspace_language(&root))
            .ok_or_else(|| LspError::UnsupportedFile { path: root.clone() })?;
        let client = self.client_for(kind, &root)?;
        client.workspace_symbols(query, self.req_timeout)
    }

    fn classify_file(&self, abs_file: &Path) -> Result<(LanguageKind, PathBuf)> {
        let kind = LanguageKind::from_path(abs_file).ok_or_else(|| LspError::UnsupportedFile {
            path: abs_file.to_path_buf(),
        })?;
        let root = discover_workspace_root(abs_file, self.default_workspace.as_deref());
        Ok((kind, root))
    }

    fn client_for(&self, kind: LanguageKind, root: &Path) -> Result<Arc<LspClient>> {
        self.evict_stale();
        let key = (kind, root.to_path_buf());
        {
            let map = self.clients.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(client) = map.get(&key) {
                if client.is_alive() {
                    return Ok(Arc::clone(client));
                }
            }
        }
        let cmd = self.resolve_server_command(kind, root)?;
        let client = Arc::new(LspClient::start(kind, root.to_path_buf(), &cmd, self.init_timeout)?);
        let replaced = {
            let mut map = self.clients.lock().unwrap_or_else(|e| e.into_inner());
            map.insert(key, Arc::clone(&client))
        };
        // A dead server for the same key: shut it down outside the lock.
        if let Some(old) = replaced {
            old.shutdown();
        }
        Ok(client)
    }

    fn resolve_server_command(&self, kind: LanguageKind, root: &Path) -> Result<ServerCommand> {
        if let Some(over) = self.overrides.get(&kind) {
            return Ok(over.clone());
        }
        let tool_name = kind.server_tool();
        let program = managed_tool(tool_name)
            .and_then(|t| find_real_tool(&self.layout, t))
            .or_else(|| find_on_path(tool_name))
            .ok_or_else(|| LspError::ServerNotFound {
                server: tool_name.to_string(),
            })?;

        let mut args = Vec::new();
        if kind == LanguageKind::Cpp {
            args.push("--background-index".to_string());
            if let Some(cc_dir) = find_compile_commands_dir(root) {
                args.push(format!("--compile-commands-dir={}", cc_dir.display()));
            }
        }

        let env = ToolEnv::for_layout(&self.layout).vars;
        Ok(ServerCommand { program, args, env })
    }
}

impl Drop for LspPool {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

fn resolve_file(file: &Path, default_workspace: Option<&Path>) -> Result<PathBuf> {
    let candidate = if file.is_absolute() {
        file.to_path_buf()
    } else if let Some(ws) = default_workspace {
        ws.join(file)
    } else {
        std::path::absolute(file).map_err(|e| LspError::InvalidPath(format!("{}: {e}", file.display())))?
    };
    if !candidate.is_file() {
        return Err(LspError::InvalidPath(format!(
            "file does not exist: {}",
            candidate.display()
        )));
    }
    Ok(candidate)
}

fn detect_workspace_language(root: &Path) -> Option<LanguageKind> {
    if root.join("Cargo.toml").is_file() {
        return Some(LanguageKind::Rust);
    }
    if root.join("CMakeLists.txt").is_file() || root.join("compile_commands.json").is_file() {
        return Some(LanguageKind::Cpp);
    }
    None
}

fn find_compile_commands_dir(root: &Path) -> Option<PathBuf> {
    for rel in ["", "build", "out/build", "cmake-build-debug"] {
        let dir = if rel.is_empty() {
            root.to_path_buf()
        } else {
            root.join(rel)
        };
        if dir.join("compile_commands.json").is_file() {
            return Some(dir);
        }
    }
    None
}

fn find_on_path(tool: &str) -> Option<PathBuf> {
    let name = format!("{tool}{}", std::env::consts::EXE_SUFFIX);
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
