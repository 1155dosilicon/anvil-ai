//! Pure, string-in/string-out edits of agent MCP configuration files.
//!
//! Only `mcpServers.anvil` (JSON) / `mcp_servers.anvil` (TOML) is ever
//! touched: every other key, server, setting and (TOML) comment is preserved.
//! A document that cannot be parsed is reported as an error and never rewritten.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table};

use crate::error::{AgentError, Result};

/// Key of the anvil entry inside the servers table.
pub const SERVER_KEY: &str = "anvil";
pub const JSON_SERVERS_KEY: &str = "mcpServers";
pub const TOML_SERVERS_KEY: &str = "mcp_servers";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigFormat {
    Json,
    Toml,
}

impl ConfigFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            ConfigFormat::Json => "JSON",
            ConfigFormat::Toml => "TOML",
        }
    }
}

/// The MCP server entry anvil registers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpEntry {
    /// Absolute path of `ANVIL_HOME/bin/anvil(.exe)`.
    pub command: String,
    pub args: Vec<String>,
    /// Environment variables injected into the MCP server process (e.g. `ANVIL_HOME`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Emit `"type": "stdio"` (Claude Code CLI).
    pub stdio_type: bool,
}

impl McpEntry {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: vec!["mcp".into()],
            env: BTreeMap::new(),
            stdio_type: false,
        }
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn with_stdio_type(mut self) -> Self {
        self.stdio_type = true;
        self
    }

    /// The entry as a JSON object (`type` first when present).
    pub fn to_json(&self) -> Value {
        let mut obj = Map::new();
        if self.stdio_type {
            obj.insert("type".into(), Value::from("stdio"));
        }
        obj.insert("command".into(), Value::from(self.command.as_str()));
        obj.insert("args".into(), Value::from(self.args.clone()));
        if !self.env.is_empty() {
            let mut env_obj = Map::new();
            for (k, v) in &self.env {
                env_obj.insert(k.clone(), Value::from(v.as_str()));
            }
            obj.insert("env".into(), Value::Object(env_obj));
        }
        Value::Object(obj)
    }
}

/// `Some(new_text)` if the document must change, `None` if it is already up to date.
pub type Edit = Option<String>;

fn invalid(path: &Path, format: ConfigFormat, message: impl ToString) -> AgentError {
    AgentError::InvalidConfig {
        path: path.to_path_buf(),
        format: format.as_str(),
        message: message.to_string(),
    }
}

fn structure(path: &Path, message: impl Into<String>) -> AgentError {
    AgentError::UnexpectedStructure {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

fn parse_json_root(path: &Path, text: Option<&str>) -> Result<Map<String, Value>> {
    let text = text.unwrap_or("");
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(structure(path, "the top-level JSON value is not an object")),
        Err(e) => Err(invalid(path, ConfigFormat::Json, e)),
    }
}

fn render_json(map: Map<String, Value>) -> String {
    let mut text = serde_json::to_string_pretty(&Value::Object(map)).expect("JSON values always serialize");
    text.push('\n');
    text
}

/// Insert or update `mcpServers.anvil`. Extra keys a user added to the anvil
/// entry (`disabled`, extra `env` keys, ...) are kept; `command`/`args`/`type`
/// and anvil's own `env` entries are enforced.
pub fn json_upsert(path: &Path, text: Option<&str>, entry: &McpEntry) -> Result<Edit> {
    let original = parse_json_root(path, text)?;
    let mut root = original.clone();
    let servers = root
        .entry(JSON_SERVERS_KEY)
        .or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(servers) = servers else {
        return Err(structure(path, format!("`{JSON_SERVERS_KEY}` is not an object")));
    };
    let desired = entry.to_json();
    match servers.get_mut(SERVER_KEY) {
        Some(Value::Object(existing)) => {
            for (k, v) in desired.as_object().expect("entry is an object") {
                if k == "env" {
                    if let Some(desired_env) = v.as_object() {
                        match existing.get_mut("env") {
                            Some(Value::Object(existing_env)) => {
                                for (ek, ev) in desired_env {
                                    if existing_env.get(ek) != Some(ev) {
                                        existing_env.insert(ek.clone(), ev.clone());
                                    }
                                }
                            }
                            _ => {
                                existing.insert(k.clone(), v.clone());
                            }
                        }
                        continue;
                    }
                }
                if existing.get(k) != Some(v) {
                    existing.insert(k.clone(), v.clone());
                }
            }
        }
        _ => {
            servers.insert(SERVER_KEY.into(), desired);
        }
    }
    let changed = text.is_none_or(|t| t.trim().is_empty()) || root != original;
    Ok(changed.then(|| render_json(root)))
}

/// Remove `mcpServers.anvil` (nothing else).
pub fn json_remove(path: &Path, text: &str) -> Result<Edit> {
    let mut root = parse_json_root(path, Some(text))?;
    let removed = match root.get_mut(JSON_SERVERS_KEY) {
        Some(Value::Object(servers)) => servers.shift_remove(SERVER_KEY).is_some(),
        Some(_) => return Err(structure(path, format!("`{JSON_SERVERS_KEY}` is not an object"))),
        None => false,
    };
    Ok(removed.then(|| render_json(root)))
}

/// `command` of the anvil entry, if one is configured.
pub fn json_inspect(path: &Path, text: &str) -> Result<Option<String>> {
    let root = parse_json_root(path, Some(text))?;
    let servers = match root.get(JSON_SERVERS_KEY) {
        None => return Ok(None),
        Some(Value::Object(servers)) => servers,
        Some(_) => return Err(structure(path, format!("`{JSON_SERVERS_KEY}` is not an object"))),
    };
    Ok(servers
        .get(SERVER_KEY)
        .map(|e| e.get("command").and_then(Value::as_str).unwrap_or_default().to_string()))
}

// ---------------------------------------------------------------------------
// TOML (Codex), via toml_edit to keep comments and formatting
// ---------------------------------------------------------------------------

fn parse_toml(path: &Path, text: &str) -> Result<DocumentMut> {
    text.parse::<DocumentMut>()
        .map_err(|e| invalid(path, ConfigFormat::Toml, e.to_string().trim()))
}

fn string_array(items: &[String]) -> Array {
    items.iter().map(String::as_str).collect()
}

fn inline_env_table(env: &BTreeMap<String, String>) -> InlineTable {
    let mut tbl = InlineTable::new();
    for (k, v) in env {
        tbl.insert(k, v.as_str().into());
    }
    tbl
}

/// Insert or update `[mcp_servers.anvil]`, preserving everything else.
pub fn toml_upsert(path: &Path, text: Option<&str>, entry: &McpEntry) -> Result<Edit> {
    let original = text.unwrap_or("");
    let mut doc = parse_toml(path, original)?;
    let root = doc.as_table_mut();
    if !root.contains_key(TOML_SERVERS_KEY) {
        let mut servers = Table::new();
        servers.set_implicit(true); // no bare `[mcp_servers]` header
        root.insert(TOML_SERVERS_KEY, Item::Table(servers));
    }
    let servers = root
        .get_mut(TOML_SERVERS_KEY)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| structure(path, format!("`{TOML_SERVERS_KEY}` is not a table")))?;

    let args = string_array(&entry.args);
    match servers.get_mut(SERVER_KEY).and_then(Item::as_table_like_mut) {
        Some(existing) => {
            if existing.get("command").and_then(Item::as_str) != Some(entry.command.as_str()) {
                existing.insert("command", toml_edit::value(entry.command.as_str()));
            }
            let current: Option<Vec<&str>> = existing
                .get("args")
                .and_then(Item::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect());
            if current != Some(entry.args.iter().map(String::as_str).collect()) {
                existing.insert("args", toml_edit::value(args));
            }
            if !entry.env.is_empty() {
                match existing.get_mut("env").and_then(Item::as_table_like_mut) {
                    Some(existing_env) => {
                        for (k, v) in &entry.env {
                            if existing_env.get(k).and_then(Item::as_str) != Some(v.as_str()) {
                                existing_env.insert(k, toml_edit::value(v.as_str()));
                            }
                        }
                    }
                    None => {
                        existing.insert("env", toml_edit::value(inline_env_table(&entry.env)));
                    }
                }
            }
        }
        None => {
            let mut table = Table::new();
            table.insert("command", toml_edit::value(entry.command.as_str()));
            table.insert("args", toml_edit::value(args));
            if !entry.env.is_empty() {
                table.insert("env", toml_edit::value(inline_env_table(&entry.env)));
            }
            servers.insert(SERVER_KEY, Item::Table(table));
        }
    }
    let mut rendered = doc.to_string();
    if !rendered.is_empty() && !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    Ok((text.is_none() || rendered != original).then_some(rendered))
}

/// Remove `[mcp_servers.anvil]` (nothing else).
pub fn toml_remove(path: &Path, text: &str) -> Result<Edit> {
    let mut doc = parse_toml(path, text)?;
    let removed = match doc.as_table_mut().get_mut(TOML_SERVERS_KEY) {
        None => false,
        Some(item) => item
            .as_table_like_mut()
            .ok_or_else(|| structure(path, format!("`{TOML_SERVERS_KEY}` is not a table")))?
            .remove(SERVER_KEY)
            .is_some(),
    };
    Ok(removed.then(|| doc.to_string()))
}

pub fn toml_inspect(path: &Path, text: &str) -> Result<Option<String>> {
    let doc = parse_toml(path, text)?;
    let servers = match doc.get(TOML_SERVERS_KEY) {
        None => return Ok(None),
        Some(item) => item
            .as_table_like()
            .ok_or_else(|| structure(path, format!("`{TOML_SERVERS_KEY}` is not a table")))?,
    };
    Ok(servers
        .get(SERVER_KEY)
        .map(|e| e.get("command").and_then(Item::as_str).unwrap_or_default().to_string()))
}

// ---------------------------------------------------------------------------
// Format dispatch
// ---------------------------------------------------------------------------

pub fn upsert(format: ConfigFormat, path: &Path, text: Option<&str>, entry: &McpEntry) -> Result<Edit> {
    match format {
        ConfigFormat::Json => json_upsert(path, text, entry),
        ConfigFormat::Toml => toml_upsert(path, text, entry),
    }
}

pub fn remove(format: ConfigFormat, path: &Path, text: &str) -> Result<Edit> {
    match format {
        ConfigFormat::Json => json_remove(path, text),
        ConfigFormat::Toml => toml_remove(path, text),
    }
}

pub fn inspect(format: ConfigFormat, path: &Path, text: &str) -> Result<Option<String>> {
    match format {
        ConfigFormat::Json => json_inspect(path, text),
        ConfigFormat::Toml => toml_inspect(path, text),
    }
}

/// Stand-alone snippet for any MCP client (`anvil agents print-config`).
pub fn generic_snippet(format: ConfigFormat, entry: &McpEntry) -> String {
    match format {
        ConfigFormat::Json => {
            let mut servers = Map::new();
            servers.insert(SERVER_KEY.into(), entry.to_json());
            let mut root = Map::new();
            root.insert(JSON_SERVERS_KEY.into(), Value::Object(servers));
            render_json(root)
        }
        ConfigFormat::Toml => toml_upsert(Path::new("snippet.toml"), None, entry)
            .expect("an empty document is valid TOML")
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"C:\Users\Eve\AppData\Local\anvil\bin\anvil.exe";

    fn p() -> &'static Path {
        Path::new("config")
    }

    #[test]
    fn json_upsert_preserves_everything_else() {
        let text = r#"{
  "apiKey": "sk-secret",
  "baseURL": "https://example.invalid/v1",
  "model": "m",
  "hooks": {"preToolUse": [{"command": "echo hi"}]},
  "mcpServers": {"other": {"command": "node", "args": ["x.js"]}}
}"#;
        let entry = McpEntry::new(EXE);
        let new = json_upsert(p(), Some(text), &entry).unwrap().unwrap();
        let v: Value = serde_json::from_str(&new).unwrap();
        assert_eq!(v["apiKey"], "sk-secret");
        assert_eq!(v["baseURL"], "https://example.invalid/v1");
        assert_eq!(v["hooks"]["preToolUse"][0]["command"], "echo hi");
        assert_eq!(v["mcpServers"]["other"]["args"][0], "x.js");
        assert_eq!(v["mcpServers"]["anvil"]["command"], EXE);
        assert_eq!(v["mcpServers"]["anvil"]["args"], serde_json::json!(["mcp"]));
        assert!(v["mcpServers"]["anvil"].get("type").is_none());
        // Key order is preserved.
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["apiKey", "baseURL", "model", "hooks", "mcpServers"]);

        // Idempotent.
        assert_eq!(json_upsert(p(), Some(&new), &entry).unwrap(), None);
        assert_eq!(json_inspect(p(), &new).unwrap().as_deref(), Some(EXE));

        // Removal restores the other servers.
        let removed = json_remove(p(), &new).unwrap().unwrap();
        let v: Value = serde_json::from_str(&removed).unwrap();
        assert!(v["mcpServers"].get("anvil").is_none());
        assert!(v["mcpServers"].get("other").is_some());
        assert_eq!(v["apiKey"], "sk-secret");
        assert_eq!(json_remove(p(), &removed).unwrap(), None);
        assert_eq!(json_inspect(p(), &removed).unwrap(), None);
    }

    #[test]
    fn json_updates_stale_entry_but_keeps_user_keys() {
        let text = r#"{"mcpServers":{"anvil":{"command":"/old/anvil","args":["mcp"],"env":{"X":"1"}}}}"#;
        let entry = McpEntry::new("/new/anvil").with_stdio_type();
        let new = json_upsert(p(), Some(text), &entry).unwrap().unwrap();
        let v: Value = serde_json::from_str(&new).unwrap();
        assert_eq!(v["mcpServers"]["anvil"]["command"], "/new/anvil");
        assert_eq!(v["mcpServers"]["anvil"]["type"], "stdio");
        assert_eq!(v["mcpServers"]["anvil"]["env"]["X"], "1");
    }

    #[test]
    fn json_new_or_empty_file() {
        let entry = McpEntry::new("/a/anvil");
        for text in [None, Some(""), Some("  \n")] {
            let new = json_upsert(p(), text, &entry).unwrap().unwrap();
            let v: Value = serde_json::from_str(&new).unwrap();
            assert_eq!(
                v,
                serde_json::json!({"mcpServers": {"anvil": {"command": "/a/anvil", "args": ["mcp"]}}})
            );
        }
    }

    #[test]
    fn json_rejects_corrupt_or_unexpected_documents() {
        let entry = McpEntry::new("/a/anvil");
        let err = json_upsert(p(), Some("{\"apiKey\": \"x\","), &entry).unwrap_err();
        assert!(matches!(err, AgentError::InvalidConfig { format: "JSON", .. }), "{err}");
        assert!(err.to_string().contains("left untouched"));
        assert!(matches!(
            json_upsert(p(), Some("[1, 2]"), &entry).unwrap_err(),
            AgentError::UnexpectedStructure { .. }
        ));
        assert!(matches!(
            json_upsert(p(), Some(r#"{"mcpServers": []}"#), &entry).unwrap_err(),
            AgentError::UnexpectedStructure { .. }
        ));
        assert!(json_remove(p(), "{oops").is_err());
        assert!(json_remove(p(), r#"{"mcpServers": 1}"#).is_err());
        assert!(json_inspect(p(), r#"{"mcpServers": 1}"#).is_err());
        assert_eq!(json_inspect(p(), r#"{"other": 1}"#).unwrap(), None);
    }

    const CODEX: &str = r#"# Codex configuration
model = "gpt-5" # inline comment

[mcp_servers.docs]
command = "npx"   # keep spacing
args = ["-y", "docs-mcp"]

# trailing section
[profiles.fast]
model = "mini"
"#;

    #[test]
    fn toml_upsert_preserves_comments_and_sections() {
        let entry = McpEntry::new(EXE);
        let new = toml_upsert(p(), Some(CODEX), &entry).unwrap().unwrap();
        // Every original line is kept verbatim and in order; the new table is
        // grouped with its sibling `[mcp_servers.docs]`.
        let mut new_lines = new.lines();
        for line in CODEX.lines() {
            assert!(new_lines.any(|l| l == line), "line {line:?} lost or reordered:\n{new}");
        }
        assert!(
            new.find("[mcp_servers.anvil]").unwrap() < new.find("[profiles.fast]").unwrap(),
            "{new}"
        );
        let doc: DocumentMut = new.parse().unwrap();
        assert_eq!(doc["mcp_servers"]["anvil"]["command"].as_str(), Some(EXE));
        assert_eq!(doc["mcp_servers"]["anvil"]["args"][0].as_str(), Some("mcp"));
        assert_eq!(doc["mcp_servers"]["docs"]["command"].as_str(), Some("npx"));
        assert!(!new.contains("\n[mcp_servers]\n"), "no bare header: {new}");

        assert_eq!(toml_upsert(p(), Some(&new), &entry).unwrap(), None, "idempotent");
        assert_eq!(toml_inspect(p(), &new).unwrap().as_deref(), Some(EXE));

        // Update in place.
        let moved = toml_upsert(p(), Some(&new), &McpEntry::new("/new/anvil"))
            .unwrap()
            .unwrap();
        assert_eq!(moved.matches("[mcp_servers.anvil]").count(), 1);
        assert!(moved.contains("/new/anvil") && !moved.contains(EXE));
        assert!(moved.contains("# inline comment") && moved.contains("# keep spacing"));

        let removed = toml_remove(p(), &new).unwrap().unwrap();
        assert_eq!(removed, CODEX, "removal restores the original document");
        assert_eq!(toml_remove(p(), &removed).unwrap(), None);
    }

    #[test]
    fn toml_new_file_and_inline_tables() {
        let entry = McpEntry::new("/a/anvil");
        let new = toml_upsert(p(), None, &entry).unwrap().unwrap();
        assert_eq!(new, "[mcp_servers.anvil]\ncommand = \"/a/anvil\"\nargs = [\"mcp\"]\n");

        let inline = "mcp_servers = { other = { command = \"x\" } }\n";
        let new = toml_upsert(p(), Some(inline), &entry).unwrap().unwrap();
        let doc: DocumentMut = new.parse().unwrap();
        assert_eq!(doc["mcp_servers"]["anvil"]["command"].as_str(), Some("/a/anvil"));
        assert_eq!(doc["mcp_servers"]["other"]["command"].as_str(), Some("x"));
    }

    #[test]
    fn toml_rejects_corrupt_documents() {
        let entry = McpEntry::new("/a/anvil");
        let err = toml_upsert(p(), Some("model = \"x\"\n[mcp_servers\n"), &entry).unwrap_err();
        assert!(matches!(err, AgentError::InvalidConfig { format: "TOML", .. }), "{err}");
        assert!(matches!(
            toml_upsert(p(), Some("mcp_servers = 3\n"), &entry).unwrap_err(),
            AgentError::UnexpectedStructure { .. }
        ));
    }

    #[test]
    fn windows_paths_round_trip_in_toml() {
        let entry = McpEntry::new(EXE).with_env("ANVIL_HOME", r"C:\Users\Eve\AppData\Local\anvil");
        let text = generic_snippet(ConfigFormat::Toml, &entry);
        assert_eq!(toml_inspect(p(), &text).unwrap().as_deref(), Some(EXE));
        let doc: DocumentMut = text.parse().unwrap();
        assert_eq!(
            doc["mcp_servers"]["anvil"]["env"]["ANVIL_HOME"].as_str(),
            Some(r"C:\Users\Eve\AppData\Local\anvil")
        );
        let json = generic_snippet(ConfigFormat::Json, &entry);
        assert_eq!(json_inspect(p(), &json).unwrap().as_deref(), Some(EXE));
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            v["mcpServers"]["anvil"]["env"]["ANVIL_HOME"],
            r"C:\Users\Eve\AppData\Local\anvil"
        );
    }

    #[test]
    fn json_upsert_merges_env_preserving_user_vars() {
        let text =
            r#"{"mcpServers":{"anvil":{"command":"/old/anvil","args":["mcp"],"env":{"X":"1","ANVIL_HOME":"/old"}}}}"#;
        let entry = McpEntry::new("/new/bin/anvil")
            .with_env("ANVIL_HOME", "/new")
            .with_stdio_type();
        let new = json_upsert(p(), Some(text), &entry).unwrap().unwrap();
        let v: Value = serde_json::from_str(&new).unwrap();
        assert_eq!(v["mcpServers"]["anvil"]["command"], "/new/bin/anvil");
        assert_eq!(v["mcpServers"]["anvil"]["env"]["X"], "1");
        assert_eq!(v["mcpServers"]["anvil"]["env"]["ANVIL_HOME"], "/new");
        assert_eq!(json_upsert(p(), Some(&new), &entry).unwrap(), None, "idempotent");
    }

    #[test]
    fn toml_upsert_merges_env_preserving_user_vars() {
        let text = "[mcp_servers.anvil]\ncommand = \"/old/anvil\"\nargs = [\"mcp\"]\nenv = { X = \"1\", ANVIL_HOME = \"/old\" }\n";
        let entry = McpEntry::new("/new/bin/anvil").with_env("ANVIL_HOME", "/new");
        let new = toml_upsert(p(), Some(text), &entry).unwrap().unwrap();
        let doc: DocumentMut = new.parse().unwrap();
        assert_eq!(doc["mcp_servers"]["anvil"]["command"].as_str(), Some("/new/bin/anvil"));
        assert_eq!(doc["mcp_servers"]["anvil"]["env"]["X"].as_str(), Some("1"));
        assert_eq!(doc["mcp_servers"]["anvil"]["env"]["ANVIL_HOME"].as_str(), Some("/new"));
        assert_eq!(toml_upsert(p(), Some(&new), &entry).unwrap(), None, "idempotent");

        // Sub-table syntax `[mcp_servers.anvil.env]` is also merged cleanly.
        let subtable = "[mcp_servers.anvil]\ncommand = \"/new/bin/anvil\"\nargs = [\"mcp\"]\n\n[mcp_servers.anvil.env]\nFOO = \"bar\"\n";
        let merged = toml_upsert(p(), Some(subtable), &entry).unwrap().unwrap();
        let doc: DocumentMut = merged.parse().unwrap();
        assert_eq!(doc["mcp_servers"]["anvil"]["env"]["FOO"].as_str(), Some("bar"));
        assert_eq!(doc["mcp_servers"]["anvil"]["env"]["ANVIL_HOME"].as_str(), Some("/new"));
        assert_eq!(toml_upsert(p(), Some(&merged), &entry).unwrap(), None, "idempotent");
    }
}
