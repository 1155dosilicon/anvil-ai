//! `anvil env`: environment snippets for interactive shells and agents.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::str::FromStr;

use anvil_core::InstallLayout;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    PowerShell,
    Bash,
    Zsh,
    Fish,
    Json,
}

impl Shell {
    pub const NAMES: [&'static str; 5] = ["powershell", "bash", "zsh", "fish", "json"];

    /// PowerShell on Windows; on Unix the shell named by `$SHELL` (default bash).
    pub fn detect() -> Shell {
        if cfg!(windows) {
            return Shell::PowerShell;
        }
        let shell = std::env::var("SHELL").unwrap_or_default();
        match shell.rsplit('/').next().unwrap_or("") {
            "zsh" => Shell::Zsh,
            "fish" => Shell::Fish,
            "pwsh" => Shell::PowerShell,
            _ => Shell::Bash,
        }
    }
}

impl FromStr for Shell {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "powershell" | "pwsh" | "ps" => Ok(Shell::PowerShell),
            "bash" | "sh" => Ok(Shell::Bash),
            "zsh" => Ok(Shell::Zsh),
            "fish" => Ok(Shell::Fish),
            "json" => Ok(Shell::Json),
            other => Err(format!(
                "unknown shell '{other}' (expected one of: {})",
                Shell::NAMES.join(", ")
            )),
        }
    }
}

/// Session environment for interactive use: shims first on PATH plus the
/// variables build systems look at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionEnv {
    pub install_dir: PathBuf,
    pub path_prepend: Vec<PathBuf>,
    /// Ordered `NAME = value` pairs (excluding PATH).
    pub vars: Vec<(String, String)>,
}

impl SessionEnv {
    pub fn for_layout(layout: &InstallLayout) -> Self {
        let mut vars = vec![
            ("ANVIL_HOME".to_string(), layout.root().display().to_string()),
            ("CARGO_HOME".to_string(), layout.cargo_home().display().to_string()),
            ("CMAKE_GENERATOR".to_string(), "Ninja".to_string()),
        ];
        for (var, tool) in [("CC", "clang"), ("CXX", "clang++")] {
            let shim = layout.shim_path(tool);
            if shim.exists() {
                vars.push((var.to_string(), shim.display().to_string()));
            }
        }
        Self {
            install_dir: layout.root().to_path_buf(),
            path_prepend: vec![layout.bin_dir()],
            vars,
        }
    }

    /// `{ install_dir, installed, path_prepend, path_separator, vars: {NAME: value} }`.
    pub fn to_json(&self) -> serde_json::Value {
        let vars: serde_json::Map<String, serde_json::Value> = self
            .vars
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        serde_json::json!({
            "install_dir": self.install_dir,
            "installed": self.install_dir.join(anvil_core::STATE_FILE).is_file(),
            "path_prepend": self.path_prepend,
            "path_separator": if cfg!(windows) { ";" } else { ":" },
            "vars": vars,
        })
    }

    pub fn render(&self, shell: Shell) -> String {
        let mut out = String::new();
        match shell {
            Shell::Json => {
                out = serde_json::to_string_pretty(&self.to_json()).expect("serializable");
                out.push('\n');
            }
            Shell::PowerShell => {
                let ps = |s: &str| format!("'{}'", s.replace('\'', "''"));
                for (k, v) in &self.vars {
                    let _ = writeln!(out, "$env:{k} = {}", ps(v));
                }
                for dir in self.path_prepend.iter().rev() {
                    let dir = dir.display().to_string();
                    let _ = writeln!(
                        out,
                        "if (-not (($env:PATH -split [IO.Path]::PathSeparator) -contains {d})) {{ $env:PATH = {d} + [IO.Path]::PathSeparator + $env:PATH }}",
                        d = ps(&dir)
                    );
                }
            }
            Shell::Bash | Shell::Zsh => {
                for (k, v) in &self.vars {
                    let _ = writeln!(out, "export {k}={}", crate::profile::sh_quote(v));
                }
                for dir in self.path_prepend.iter().rev() {
                    let d = crate::profile::sh_quote(&dir.display().to_string());
                    let _ = writeln!(
                        out,
                        "case \":${{PATH}}:\" in *:{d}:*) ;; *) export PATH={d}\":${{PATH}}\" ;; esac"
                    );
                }
            }
            Shell::Fish => {
                let fish = |s: &str| format!("'{}'", s.replace('\\', r"\\").replace('\'', r"\'"));
                for (k, v) in &self.vars {
                    let _ = writeln!(out, "set -gx {k} {}", fish(v));
                }
                for dir in self.path_prepend.iter().rev() {
                    let d = fish(&dir.display().to_string());
                    let _ = writeln!(out, "contains -- {d} $PATH; or set -gx PATH {d} $PATH");
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> SessionEnv {
        SessionEnv::for_layout(&InstallLayout::new(PathBuf::from("/opt/it's anvil")))
    }

    #[test]
    fn parses_shell_names() {
        for name in Shell::NAMES {
            assert!(name.parse::<Shell>().is_ok());
        }
        assert_eq!("PWSH".parse::<Shell>().unwrap(), Shell::PowerShell);
        assert!("cmd".parse::<Shell>().is_err());
    }

    #[test]
    fn renders_every_shell() {
        let e = env();
        let ps = e.render(Shell::PowerShell);
        assert!(ps.contains("$env:ANVIL_HOME = '/opt/it''s anvil'"), "{ps}");
        assert!(ps.contains("$env:CMAKE_GENERATOR = 'Ninja'"));

        let sh = e.render(Shell::Bash);
        assert!(sh.contains(r"export ANVIL_HOME='/opt/it'\''s anvil'"), "{sh}");
        assert!(sh.contains("export PATH="));
        assert_eq!(sh, e.render(Shell::Zsh));

        let fish = e.render(Shell::Fish);
        assert!(fish.contains(r"set -gx ANVIL_HOME '/opt/it\'s anvil'"), "{fish}");

        let json: serde_json::Value = serde_json::from_str(&e.render(Shell::Json)).unwrap();
        assert_eq!(json["vars"]["CMAKE_GENERATOR"], "Ninja");
        assert_eq!(json["path_prepend"].as_array().unwrap().len(), 1);
        assert!(json["vars"].get("CC").is_none(), "no clang shim installed");
    }
}
