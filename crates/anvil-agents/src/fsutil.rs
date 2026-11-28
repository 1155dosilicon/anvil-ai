//! Atomic writes and backups for files outside `ANVIL_HOME`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anvil_core::error::IoContext;

use crate::error::Result;

/// Read a text file; `Ok(None)` if it does not exist. A UTF-8 BOM is stripped.
pub fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            Ok(Some(text.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(text)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).at(path).map_err(Into::into),
    }
}

/// The file that is actually written for `path`: symlinks (dotfile managers)
/// are followed so the link itself is preserved.
fn write_target(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

/// Write `contents` to `path` atomically: temp file in the same directory,
/// fsync, then rename over the destination. Readers never see a partial file.
pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let target = write_target(path);
    let dir = target.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).at(dir)?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".anvil-tmp-")
        .tempfile_in(dir)
        .at(dir)?;
    tmp.write_all(contents).at(tmp.path())?;
    tmp.as_file().sync_all().at(tmp.path())?;
    tmp.persist(&target).map_err(|e| e.error).at(&target)?;
    Ok(())
}

/// `20261127T161230Z` (UTC, filesystem-safe).
pub fn timestamp() -> String {
    anvil_core::state::now_rfc3339().replace(['-', ':'], "")
}

/// Copy `path` to `<backups_dir>/<agent>-<file name>-<timestamp>.bak`
/// (with a numeric suffix if that name is taken). Returns the backup path.
pub fn backup(backups_dir: &Path, agent: &str, path: &Path) -> Result<PathBuf> {
    fs::create_dir_all(backups_dir).at(backups_dir)?;
    let file = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config".into());
    let stem = format!("{agent}-{file}-{}", timestamp());
    let mut dest = backups_dir.join(format!("{stem}.bak"));
    let mut n = 1;
    while dest.exists() {
        dest = backups_dir.join(format!("{stem}-{n}.bak"));
        n += 1;
    }
    fs::copy(path, &dest).at(&dest)?;
    Ok(dest)
}

/// Remove `dir` if it exists and is empty.
pub fn remove_dir_if_empty(dir: &Path) {
    if fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_none()) {
        let _ = fs::remove_dir(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec!["config.json"]);
    }

    #[test]
    fn backups_never_collide() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(".claude.json");
        fs::write(&src, "{}").unwrap();
        let backups = dir.path().join("backups");
        let a = backup(&backups, "claude", &src).unwrap();
        let b = backup(&backups, "claude", &src).unwrap();
        assert_ne!(a, b);
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("claude-.claude.json-") && name.ends_with(".bak"),
            "{name}"
        );
        assert_eq!(fs::read_to_string(b).unwrap(), "{}");
    }

    #[test]
    fn bom_is_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        fs::write(&path, "\u{feff}{}").unwrap();
        assert_eq!(read_optional(&path).unwrap().as_deref(), Some("{}"));
        assert_eq!(read_optional(&dir.path().join("missing")).unwrap(), None);
    }
}
