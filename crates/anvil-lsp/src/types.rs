//! Agent-friendly (1-based line/column) LSP result types and language helpers.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Supported LSP server families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LanguageKind {
    Rust,
    Cpp,
}

impl LanguageKind {
    /// Detect language family from a source file extension.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "rs" => Some(Self::Rust),
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hxx" | "hh" | "m" | "mm" => Some(Self::Cpp),
            _ => None,
        }
    }

    /// Parse explicit language hint (`"rust"`, `"c"`, `"cpp"`, `"c++"`).
    pub fn parse_hint(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "rs" | "rust" | "rust-analyzer" => Some(Self::Rust),
            "c" | "cc" | "cpp" | "c++" | "cxx" | "clangd" => Some(Self::Cpp),
            _ => None,
        }
    }

    /// LSP `languageId` for a specific file.
    pub fn lsp_language_id(self, path: &Path) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Cpp => match path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("c") => "c",
                Some("m") => "objective-c",
                Some("mm") => "objective-cpp",
                _ => "cpp",
            },
        }
    }

    /// Managed Anvil tool name for the server binary.
    pub fn server_tool(self) -> &'static str {
        match self {
            Self::Rust => "rust-analyzer",
            Self::Cpp => "clangd",
        }
    }
}

/// Find the best workspace root for a source file (nearest Cargo.toml / CMakeLists.txt / compile_commands.json / .git).
pub fn discover_workspace_root(file: &Path, fallback: Option<&Path>) -> PathBuf {
    let start = if file.is_dir() {
        file
    } else {
        file.parent().unwrap_or(file)
    };
    let markers: &[&str] = match LanguageKind::from_path(file) {
        Some(LanguageKind::Rust) => &["Cargo.toml", ".git"],
        Some(LanguageKind::Cpp) => &["compile_commands.json", "CMakeLists.txt", ".clangd", ".git"],
        None => &["Cargo.toml", "CMakeLists.txt", "compile_commands.json", ".git"],
    };

    for marker in markers {
        for ancestor in start.ancestors() {
            if ancestor.join(marker).exists() {
                // For Cargo workspace: prefer the topmost ancestor with Cargo.toml that has `[workspace]`.
                if *marker == "Cargo.toml" {
                    let mut best = ancestor.to_path_buf();
                    for higher in ancestor.ancestors().skip(1) {
                        let cargo = higher.join("Cargo.toml");
                        if cargo.is_file() {
                            if let Ok(text) = std::fs::read_to_string(&cargo) {
                                if text.contains("[workspace]") {
                                    best = higher.to_path_buf();
                                }
                            }
                        }
                    }
                    return best;
                }
                // `add_subdirectory()` projects: the root is the topmost directory of
                // the unbroken chain of `CMakeLists.txt` files above the source.
                if *marker == "CMakeLists.txt" {
                    let mut best = ancestor;
                    for higher in ancestor.ancestors().skip(1) {
                        if !higher.join(marker).is_file() {
                            break;
                        }
                        best = higher;
                    }
                    return best.to_path_buf();
                }
                return ancestor.to_path_buf();
            }
        }
    }

    fallback.map(Path::to_path_buf).unwrap_or_else(|| start.to_path_buf())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiagnosticItem {
    /// 1-based start line.
    pub line: u32,
    /// 1-based start column.
    pub col: u32,
    /// 1-based end line.
    pub end_line: u32,
    /// 1-based end column.
    pub end_col: u32,
    /// `"error"`, `"warning"`, `"info"`, or `"hint"`.
    pub severity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HoverInfo {
    pub contents: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocationItem {
    pub file: PathBuf,
    /// 1-based start line.
    pub line: u32,
    /// 1-based start column.
    pub col: u32,
    /// 1-based end line.
    pub end_line: u32,
    /// 1-based end column.
    pub end_col: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolItem {
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    /// 1-based line.
    pub line: u32,
    /// 1-based column.
    pub col: u32,
}

/// Convert LSP `SymbolKind` integer into a readable name.
pub fn symbol_kind_name(kind: u64) -> &'static str {
    match kind {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        15 => "string",
        16 => "number",
        17 => "boolean",
        18 => "array",
        19 => "object",
        20 => "key",
        21 => "null",
        22 => "enum_member",
        23 => "struct",
        24 => "event",
        25 => "operator",
        26 => "type_parameter",
        _ => "symbol",
    }
}

pub fn severity_name(severity: Option<u64>) -> String {
    match severity {
        Some(1) => "error".into(),
        Some(2) => "warning".into(),
        Some(3) => "info".into(),
        Some(4) => "hint".into(),
        _ => "info".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_detection_covers_rust_and_cpp() {
        assert_eq!(LanguageKind::from_path(Path::new("main.rs")), Some(LanguageKind::Rust));
        assert_eq!(LanguageKind::from_path(Path::new("foo.c")), Some(LanguageKind::Cpp));
        assert_eq!(LanguageKind::from_path(Path::new("foo.HPP")), Some(LanguageKind::Cpp));
        assert_eq!(LanguageKind::from_path(Path::new("notes.md")), None);
        assert_eq!(LanguageKind::Cpp.lsp_language_id(Path::new("a.c")), "c");
        assert_eq!(LanguageKind::Cpp.lsp_language_id(Path::new("a.cpp")), "cpp");
    }

    #[test]
    fn workspace_root_prefers_cargo_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\nmembers = [\"a\"]\n").unwrap();
        let member = dir.path().join("a").join("src");
        std::fs::create_dir_all(&member).unwrap();
        std::fs::write(dir.path().join("a").join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
        let file = member.join("lib.rs");
        std::fs::write(&file, "pub fn hi() {}\n").unwrap();
        assert_eq!(discover_workspace_root(&file, None), dir.path());
    }

    #[test]
    fn workspace_root_climbs_nested_cmake_projects() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        let lib = root.join("libs").join("core");
        std::fs::create_dir_all(lib.join("src")).unwrap();
        std::fs::write(root.join("CMakeLists.txt"), "add_subdirectory(libs)\n").unwrap();
        std::fs::write(root.join("libs").join("CMakeLists.txt"), "add_subdirectory(core)\n").unwrap();
        std::fs::write(lib.join("CMakeLists.txt"), "add_library(core src/a.c)\n").unwrap();
        let file = lib.join("src").join("a.c");
        std::fs::write(&file, "int a;\n").unwrap();
        assert_eq!(discover_workspace_root(&file, None), root);

        // A gap in the chain stops the climb.
        std::fs::remove_file(root.join("libs").join("CMakeLists.txt")).unwrap();
        assert_eq!(discover_workspace_root(&file, None), lib);
    }
}
