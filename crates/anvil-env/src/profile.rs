//! Linux user environment files.
//!
//! * `~/.config/environment.d/90-anvil.conf` – read by systemd user sessions,
//!   hence by GUI applications (IDEs, desktop agents).
//! * A delimited block in `~/.profile` (created if missing) and in `~/.bashrc`,
//!   `~/.bash_profile`, `~/.zshrc` (only if they exist) – for login/interactive shells.
//!
//! Everything here is plain file manipulation rooted at explicit directories,
//! so it is compiled and tested on every platform without touching `$HOME`.

use std::fs;
use std::path::{Path, PathBuf};

use anvil_core::error::IoContext;
use anvil_core::InstallLayout;

use crate::error::Result;
use crate::userenv::EnvChange;

pub const BLOCK_START: &str = "# >>> anvil >>>";
pub const BLOCK_END: &str = "# <<< anvil <<<";
pub const ENVIRONMENT_D_FILE: &str = "90-anvil.conf";

/// Where the Linux environment files live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxEnvPaths {
    pub home: PathBuf,
    /// `$XDG_CONFIG_HOME` (usually `~/.config`).
    pub config_dir: PathBuf,
}

impl LinuxEnvPaths {
    pub fn new(home: impl Into<PathBuf>, config_dir: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            config_dir: config_dir.into(),
        }
    }

    pub fn environment_d_file(&self) -> PathBuf {
        self.config_dir.join("environment.d").join(ENVIRONMENT_D_FILE)
    }

    pub fn profile(&self) -> PathBuf {
        self.home.join(".profile")
    }

    /// Shell rc files that are only edited when they already exist.
    pub fn optional_rc_files(&self) -> Vec<PathBuf> {
        [".bash_profile", ".bashrc", ".zshrc"]
            .iter()
            .map(|f| self.home.join(f))
            .collect()
    }
}

/// POSIX single-quoting.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The managed shell block (including the start/end markers, no trailing newline).
pub fn render_profile_block(layout: &InstallLayout) -> String {
    let home = layout.root().display().to_string();
    let bin = layout.bin_dir().display().to_string();
    [
        BLOCK_START.to_string(),
        "# Managed by anvil; removed by `anvil uninstall`. Do not edit inside this block.".to_string(),
        format!("export ANVIL_HOME={}", sh_quote(&home)),
        "case \":${PATH}:\" in".to_string(),
        format!("  *:{}:*) ;;", sh_quote(&bin)),
        format!("  *) export PATH={}\":${{PATH}}\" ;;", sh_quote(&bin)),
        "esac".to_string(),
        BLOCK_END.to_string(),
    ]
    .join("\n")
}

/// Quote a value for `environment.d(5)` when needed.
///
/// Plain values (letters, digits and `/ . _ - + , @ % ~ = :`) are emitted
/// verbatim; anything else (spaces, quotes, `\`, `#`, `;`, non-ASCII, ...) is
/// wrapped in double quotes with `\` and `"` backslash-escaped. `${VAR}`
/// references outside the quoted part are still expanded by systemd.
pub fn environment_d_quote(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+,@%~=:".contains(c));
    if plain {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', r"\\").replace('"', "\\\""))
    }
}

/// Contents of `environment.d/90-anvil.conf`.
pub fn render_environment_d(layout: &InstallLayout) -> String {
    let home = layout.root().display().to_string();
    let bin = layout.bin_dir().display().to_string();
    format!(
        "# Managed by anvil; removed by `anvil uninstall`.\nANVIL_HOME={}\nPATH={}:${{PATH}}\n",
        environment_d_quote(&home),
        environment_d_quote(&bin)
    )
}

/// Line range `[start, end]` of the managed block, if complete.
fn block_range(lines: &[&str]) -> Option<(usize, usize)> {
    let start = lines.iter().position(|l| l.trim_end() == BLOCK_START)?;
    let end = lines[start..].iter().position(|l| l.trim_end() == BLOCK_END)? + start;
    Some((start, end))
}

pub fn has_block(content: &str) -> bool {
    block_range(&content.lines().collect::<Vec<_>>()).is_some()
}

/// Insert `block` or replace an existing managed block in place.
pub fn upsert_block(content: &str, block: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if let Some((start, end)) = block_range(&lines) {
        let mut out: Vec<&str> = lines[..start].to_vec();
        out.extend(block.lines());
        out.extend(&lines[end + 1..]);
        return out.join("\n") + "\n";
    }
    // Drop an orphaned start marker (block truncated by hand) before appending.
    let kept: Vec<&str> = lines.into_iter().filter(|l| l.trim_end() != BLOCK_START).collect();
    if kept.is_empty() {
        return format!("{block}\n");
    }
    format!("{}\n\n{block}\n", kept.join("\n"))
}

/// Remove the managed block (and the blank separator line added before it).
pub fn remove_block(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let Some((mut start, end)) = block_range(&lines) else {
        return content.to_string();
    };
    if start > 0 && lines[start - 1].trim().is_empty() {
        start -= 1;
    }
    let out: Vec<&str> = lines[..start].iter().chain(&lines[end + 1..]).copied().collect();
    if out.is_empty() {
        String::new()
    } else {
        out.join("\n") + "\n"
    }
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).at(path).map_err(Into::into),
    }
}

fn write_if_changed(path: &Path, old: Option<&str>, new: &str, change: &mut EnvChange) -> Result<()> {
    change.targets.push(path.display().to_string());
    if old == Some(new) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).at(dir)?;
    }
    fs::write(path, new).at(path)?;
    change.modified.push(path.display().to_string());
    Ok(())
}

/// Write `environment.d` and the shell blocks. Idempotent.
pub fn apply_linux_env(paths: &LinuxEnvPaths, layout: &InstallLayout) -> Result<EnvChange> {
    let mut change = EnvChange::default();

    let env_d = paths.environment_d_file();
    let old = read_optional(&env_d)?;
    write_if_changed(&env_d, old.as_deref(), &render_environment_d(layout), &mut change)?;

    let block = render_profile_block(layout);
    let profile = paths.profile();
    let old = read_optional(&profile)?;
    let new = upsert_block(old.as_deref().unwrap_or(""), &block);
    write_if_changed(&profile, old.as_deref(), &new, &mut change)?;

    for rc in paths.optional_rc_files() {
        if let Some(old) = read_optional(&rc)? {
            let new = upsert_block(&old, &block);
            write_if_changed(&rc, Some(&old), &new, &mut change)?;
        }
    }
    Ok(change)
}

/// Remove `environment.d/90-anvil.conf` and every managed block. Idempotent.
pub fn remove_linux_env(paths: &LinuxEnvPaths) -> Result<EnvChange> {
    let mut change = EnvChange::default();
    let env_d = paths.environment_d_file();
    change.targets.push(env_d.display().to_string());
    if env_d.exists() {
        fs::remove_file(&env_d).at(&env_d)?;
        change.modified.push(env_d.display().to_string());
    }
    for file in std::iter::once(paths.profile()).chain(paths.optional_rc_files()) {
        if let Some(old) = read_optional(&file)? {
            if has_block(&old) {
                write_if_changed(&file, Some(&old), &remove_block(&old), &mut change)?;
            }
        }
    }
    Ok(change)
}

/// `true` when both `environment.d` and the `~/.profile` block point to this install.
pub fn linux_env_status(paths: &LinuxEnvPaths, layout: &InstallLayout) -> Result<bool> {
    let env_d = read_optional(&paths.environment_d_file())?;
    let profile = read_optional(&paths.profile())?;
    let block = render_profile_block(layout);
    Ok(env_d.as_deref() == Some(render_environment_d(layout).as_str()) && profile.is_some_and(|p| p.contains(&block)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, LinuxEnvPaths, InstallLayout) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let paths = LinuxEnvPaths::new(&home, home.join(".config"));
        let layout = InstallLayout::new(home.join(".local/share/anvil"));
        (dir, paths, layout)
    }

    fn count(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    #[test]
    fn block_upsert_and_remove_are_idempotent() {
        let original = "# user stuff\nexport EDITOR=vim\n";
        let block = format!("{BLOCK_START}\nexport A=1\n{BLOCK_END}");
        let once = upsert_block(original, &block);
        let twice = upsert_block(&once, &block);
        assert_eq!(once, twice);
        assert_eq!(count(&once, BLOCK_START), 1);
        assert!(once.starts_with(original));

        // Replacing with a new block updates in place.
        let block2 = format!("{BLOCK_START}\nexport A=2\n{BLOCK_END}");
        let updated = upsert_block(&format!("{once}# trailing\n"), &block2);
        assert_eq!(count(&updated, BLOCK_START), 1);
        assert!(updated.contains("A=2") && !updated.contains("A=1"));
        assert!(updated.ends_with("# trailing\n"));

        assert_eq!(remove_block(&once), original);
        assert_eq!(remove_block(original), original);
        assert_eq!(remove_block(&upsert_block("", &block)), "");
    }

    #[test]
    fn orphaned_marker_is_repaired() {
        let broken = format!("a\n{BLOCK_START}\nb\n");
        let fixed = upsert_block(&broken, &format!("{BLOCK_START}\nx\n{BLOCK_END}"));
        assert_eq!(count(&fixed, BLOCK_START), 1);
        assert!(fixed.contains("\nb\n"));
    }

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("/a b"), "'/a b'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn environment_d_quoting() {
        assert_eq!(
            environment_d_quote("/home/u/.local/share/anvil"),
            "/home/u/.local/share/anvil"
        );
        assert_eq!(
            environment_d_quote("/home/u/My Tools/anvil"),
            "\"/home/u/My Tools/anvil\""
        );
        assert_eq!(environment_d_quote(r#"/a"b\c"#), r#""/a\"b\\c""#);
        assert_eq!(environment_d_quote("/x#y;z"), "\"/x#y;z\"");
        assert_eq!(environment_d_quote("/çà"), "\"/çà\"");
        assert_eq!(environment_d_quote(""), "\"\"");

        let layout = InstallLayout::new(r#"/home/u/My "Tools"/anvil"#);
        let rendered = render_environment_d(&layout);
        let bin = layout
            .bin_dir()
            .display()
            .to_string()
            .replace('\\', r"\\")
            .replace('"', "\\\"");
        assert!(
            rendered.contains("\nANVIL_HOME=\"/home/u/My \\\"Tools\\\"/anvil\"\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains(&format!("\nPATH=\"{bin}\":${{PATH}}\n")),
            "{rendered}"
        );

        // Plain paths stay unquoted (byte-for-byte compatible with earlier installs).
        let plain = render_environment_d(&InstallLayout::new("/opt/anvil"));
        assert!(plain.contains("\nANVIL_HOME=/opt/anvil\n"), "{plain}");
    }

    #[test]
    fn apply_creates_profile_and_environment_d_without_duplicates() {
        let (_tmp, paths, layout) = setup();
        let bashrc = paths.home.join(".bashrc");
        fs::write(&bashrc, "alias ll='ls -l'\n").unwrap();

        let first = apply_linux_env(&paths, &layout).unwrap();
        assert!(first.changed());
        assert_eq!(first.modified.len(), 3, "{first:?}"); // environment.d, .profile, .bashrc
        assert!(!paths.home.join(".zshrc").exists(), ".zshrc must not be created");
        assert!(!paths.home.join(".bash_profile").exists());

        let env_d = fs::read_to_string(paths.environment_d_file()).unwrap();
        let quoted = |p: &Path| environment_d_quote(&p.display().to_string());
        assert!(env_d.contains(&format!("PATH={}:${{PATH}}", quoted(&layout.bin_dir()))));
        assert!(env_d.contains(&format!("ANVIL_HOME={}", quoted(layout.root()))));

        let profile = fs::read_to_string(paths.profile()).unwrap();
        assert_eq!(count(&profile, BLOCK_START), 1);
        assert_eq!(count(&profile, BLOCK_END), 1);
        assert!(profile.contains(&sh_quote(&layout.bin_dir().display().to_string())));
        assert!(linux_env_status(&paths, &layout).unwrap());

        // Second application changes nothing.
        let second = apply_linux_env(&paths, &layout).unwrap();
        assert!(!second.changed(), "{second:?}");
        assert_eq!(fs::read_to_string(paths.profile()).unwrap(), profile);
        assert_eq!(count(&fs::read_to_string(&bashrc).unwrap(), BLOCK_START), 1);

        // Removal restores user files and is itself idempotent.
        let removed = remove_linux_env(&paths).unwrap();
        assert!(removed.changed());
        assert!(!paths.environment_d_file().exists());
        assert_eq!(fs::read_to_string(&bashrc).unwrap(), "alias ll='ls -l'\n");
        assert!(!fs::read_to_string(paths.profile()).unwrap().contains(BLOCK_START));
        assert!(!linux_env_status(&paths, &layout).unwrap());
        assert!(!remove_linux_env(&paths).unwrap().changed());
    }

    #[cfg(unix)]
    #[test]
    fn generated_block_is_valid_posix_shell() {
        // Source the block twice through /bin/sh: PATH must be prepended exactly once.
        let (_tmp, _paths, layout) = setup();
        let block = render_profile_block(&layout);
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "PATH=/usr/bin:/bin\n{block}\n{block}\nprintf '%s|%s' \"$ANVIL_HOME\" \"$PATH\""
            ))
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let (home, path) = text.split_once('|').unwrap();
        assert_eq!(home, layout.root().display().to_string());
        let bin = layout.bin_dir().display().to_string();
        assert_eq!(
            path,
            format!("{bin}:/usr/bin:/bin"),
            "PATH must be prepended exactly once"
        );
    }
}
