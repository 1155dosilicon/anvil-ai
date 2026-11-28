//! Single-pass streaming extraction of `.tar.zst` bundles.
//!
//! Pipeline (one sequential read of the source file, optimised for slow USB drives):
//!
//! ```text
//! File ─► HashingReader (SHA-256) ─► Metered (progress) ─► BufReader(1 MiB)
//!      ─► zstd::Decoder ─► tar::Archive ─► <dest>/../.tmp-<id>/
//! ```
//!
//! Everything is unpacked into a sibling temporary directory on the destination
//! drive. Only when the whole archive has been read and the SHA-256 matches the
//! manifest is the temporary directory renamed onto the final destination.
//! On any error the temporary directory is removed and the destination is left
//! untouched.
//!
//! Security: entries with absolute paths, drive prefixes or `..` components are
//! rejected, as are symlinks/hard links that would resolve outside the bundle
//! and links that would be created beneath another symlink. Links are created
//! only after all regular files have been written, so no file can ever be
//! written *through* a symlink coming from the archive.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::error::{CoreError, IoContext, Result};
use crate::hash::{sha256_file, HashingReader};
use crate::paths::is_safe_component;
use crate::progress::Progress;

/// Read buffer between the source file and the zstd decoder.
const READ_BUFFER: usize = 1 << 20;
/// Progress callbacks are throttled to one per this many bytes.
const PROGRESS_GRANULARITY: u64 = 256 * 1024;

/// Parameters of [`extract_bundle_streaming`].
#[derive(Debug, Clone)]
pub struct ExtractRequest<'a> {
    /// Bundle id (used for the `.tmp-<id>` directory and error messages).
    pub id: &'a str,
    /// Path of the `.tar.zst` archive.
    pub archive: &'a Path,
    /// Expected lower-case/upper-case hex SHA-256 of the archive file.
    pub expected_sha256: &'a str,
    /// Expected archive size; checked when `Some`.
    pub expected_size: Option<u64>,
    /// Final destination directory (replaced atomically if it exists).
    pub dest: &'a Path,
}

/// Result of a successful extraction.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExtractReport {
    pub id: String,
    pub dest: PathBuf,
    pub sha256: String,
    pub archive_bytes: u64,
    pub unpacked_bytes: u64,
    pub files: u64,
    pub dirs: u64,
    pub links: u64,
    /// Non-fatal issues (e.g. a symlink replaced by a copy on Windows).
    pub warnings: Vec<String>,
}

/// SHA-256 and size of an archive produced by [`pack_dir_tar_zst`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveInfo {
    pub sha256: String,
    pub size_bytes: u64,
}

/// Extract `req.archive` into `req.dest`, verifying its SHA-256 on the fly.
pub fn extract_bundle_streaming(req: &ExtractRequest<'_>, progress: &mut dyn Progress) -> Result<ExtractReport> {
    if !is_safe_component(req.id) {
        return Err(CoreError::InvalidManifest(format!("invalid bundle id '{}'", req.id)));
    }
    let parent = req
        .dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| CoreError::Environment(format!("parent of '{}'", req.dest.display())))?;
    fs::create_dir_all(parent).at(parent)?;

    let tmp = parent.join(format!(".tmp-{}", req.id));
    remove_path_if_exists(&tmp)?;
    fs::create_dir(&tmp).at(&tmp)?;

    let result = unpack_verified(req, &tmp, progress).and_then(|report| {
        commit_dir(&tmp, req.dest, req.id)?;
        Ok(report)
    });
    match result {
        Ok(mut report) => {
            report.dest = req.dest.to_path_buf();
            progress.finish(req.id);
            Ok(report)
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            Err(e)
        }
    }
}

/// Reader that forwards byte counts to a [`Progress`] reporter.
struct Metered<'p, R> {
    inner: R,
    progress: &'p mut dyn Progress,
    done: u64,
    last_reported: u64,
}

impl<R: Read> Read for Metered<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.done += n as u64;
        if n == 0 || self.done - self.last_reported >= PROGRESS_GRANULARITY {
            self.last_reported = self.done;
            self.progress.advance(self.done);
        }
        Ok(n)
    }
}

enum DeferredLink {
    Symlink { rel: PathBuf, target: PathBuf },
    Hard { rel: PathBuf, source: PathBuf },
}

fn unpack_verified(req: &ExtractRequest<'_>, tmp: &Path, progress: &mut dyn Progress) -> Result<ExtractReport> {
    let id = req.id;
    let archive_err = |source: io::Error| CoreError::Archive {
        id: id.to_string(),
        source,
    };
    let unsafe_entry = |entry: &Path, reason: &str| CoreError::UnsafeEntry {
        id: id.to_string(),
        entry: entry.display().to_string(),
        reason: reason.to_string(),
    };

    let file = File::open(req.archive).at(req.archive)?;
    let total = match req.expected_size {
        Some(size) => size,
        None => file.metadata().at(req.archive)?.len(),
    };
    progress.begin(id, total);

    let metered = Metered {
        inner: HashingReader::new(file),
        progress,
        done: 0,
        last_reported: 0,
    };
    let decoder = zstd::stream::read::Decoder::with_buffer(BufReader::with_capacity(READ_BUFFER, metered))
        .map_err(archive_err)?;
    let mut archive = tar::Archive::new(decoder);
    archive.set_preserve_mtime(true);
    archive.set_unpack_xattrs(false);
    archive.set_overwrite(true);

    let mut report = ExtractReport {
        id: id.to_string(),
        dest: req.dest.to_path_buf(),
        sha256: String::new(),
        archive_bytes: 0,
        unpacked_bytes: 0,
        files: 0,
        dirs: 0,
        links: 0,
        warnings: Vec::new(),
    };
    let mut deferred = Vec::new();

    for entry in archive.entries().map_err(archive_err)? {
        let mut entry = entry.map_err(archive_err)?;
        let raw_path = entry.path().map_err(archive_err)?.into_owned();
        let rel = match sanitize_entry_path(&raw_path) {
            Ok(Some(rel)) => rel,
            Ok(None) => continue, // "./" root entry
            Err(reason) => return Err(unsafe_entry(&raw_path, reason)),
        };
        let target = tmp.join(&rel);

        use tar::EntryType as T;
        match entry.header().entry_type() {
            T::Directory => {
                fs::create_dir_all(&target).at(&target)?;
                report.dirs += 1;
            }
            T::Regular | T::Continuous | T::GNUSparse => {
                if let Some(dir) = target.parent() {
                    fs::create_dir_all(dir).at(dir)?;
                }
                report.unpacked_bytes += entry.size();
                // Errors here are usually stream corruption (the tar crate's message
                // already names the entry and the destination).
                entry.unpack(&target).map_err(archive_err)?;
                report.files += 1;
            }
            T::Symlink => {
                let link = entry
                    .link_name()
                    .map_err(archive_err)?
                    .ok_or_else(|| unsafe_entry(&raw_path, "symlink without target"))?
                    .into_owned();
                check_symlink_target(&rel, &link).map_err(|reason| unsafe_entry(&raw_path, reason))?;
                deferred.push(DeferredLink::Symlink { rel, target: link });
            }
            T::Link => {
                let link = entry
                    .link_name()
                    .map_err(archive_err)?
                    .ok_or_else(|| unsafe_entry(&raw_path, "hard link without target"))?
                    .into_owned();
                let source = match sanitize_entry_path(&link) {
                    Ok(Some(source)) => source,
                    Ok(None) => return Err(unsafe_entry(&raw_path, "hard link to the archive root")),
                    Err(reason) => return Err(unsafe_entry(&raw_path, reason)),
                };
                deferred.push(DeferredLink::Hard { rel, source });
            }
            // Metadata records are normally consumed by the tar crate itself.
            T::XGlobalHeader | T::XHeader | T::GNULongName | T::GNULongLink => {}
            other => {
                return Err(unsafe_entry(
                    &raw_path,
                    &format!("unsupported entry type {other:?} (devices and FIFOs are not allowed)"),
                ))
            }
        }
    }

    // Drain the rest of the decompressed stream (tar padding / zstd frame
    // checksum), then the rest of the raw file so the hash covers every byte.
    let mut decoder = archive.into_inner();
    io::copy(&mut decoder, &mut io::sink()).map_err(archive_err)?;
    let mut buffered = decoder.finish();
    io::copy(&mut buffered, &mut io::sink()).at(req.archive)?;
    let metered = buffered.into_inner();
    metered.progress.advance(metered.done);
    let (actual, bytes) = metered.inner.finalize();

    if !actual.eq_ignore_ascii_case(req.expected_sha256) {
        return Err(CoreError::ChecksumMismatch {
            id: id.to_string(),
            expected: req.expected_sha256.to_ascii_lowercase(),
            actual,
        });
    }
    if let Some(expected) = req.expected_size {
        if expected != bytes {
            return Err(CoreError::SizeMismatch {
                id: id.to_string(),
                expected,
                actual: bytes,
            });
        }
    }
    report.sha256 = actual;
    report.archive_bytes = bytes;

    // On Windows a symlink must know whether its target is a directory, and the
    // copy fallback needs the target to exist: links pointing at other links
    // are retried once their target has been created (archive order is arbitrary).
    let mut pending: Vec<(PathBuf, PathBuf)> = Vec::new();
    for link in deferred {
        match link {
            DeferredLink::Symlink { rel, target } => {
                if !symlink_target_exists(tmp, &rel, &target) {
                    pending.push((rel, target));
                    continue;
                }
                ensure_no_symlink_ancestors(tmp, &rel).map_err(|r| unsafe_entry(&rel, r))?;
                create_symlink(id, tmp, &rel, &target, &mut report.warnings)?;
            }
            DeferredLink::Hard { rel, source } => {
                ensure_no_symlink_ancestors(tmp, &rel).map_err(|r| unsafe_entry(&rel, r))?;
                ensure_no_symlink_ancestors(tmp, &source).map_err(|r| unsafe_entry(&rel, r))?;
                let src = tmp.join(&source);
                let dst = tmp.join(&rel);
                if let Some(dir) = dst.parent() {
                    fs::create_dir_all(dir).at(dir)?;
                }
                remove_path_if_exists(&dst)?;
                if fs::hard_link(&src, &dst).is_err() {
                    fs::copy(&src, &dst).at(&dst)?;
                }
            }
        }
        report.links += 1;
    }
    while !pending.is_empty() {
        let (ready, waiting): (Vec<_>, Vec<_>) = pending
            .into_iter()
            .partition(|(rel, target)| symlink_target_exists(tmp, rel, target));
        // No progress: the rest is dangling (or cyclic); create_symlink reports it.
        let (batch, rest) = if ready.is_empty() {
            (waiting, Vec::new())
        } else {
            (ready, waiting)
        };
        for (rel, target) in batch {
            ensure_no_symlink_ancestors(tmp, &rel).map_err(|r| unsafe_entry(&rel, r))?;
            create_symlink(id, tmp, &rel, &target, &mut report.warnings)?;
            report.links += 1;
        }
        pending = rest;
    }

    Ok(report)
}

fn symlink_target_exists(root: &Path, rel: &Path, target: &Path) -> bool {
    root.join(rel).parent().unwrap_or(root).join(target).exists()
}

/// Normalise a tar entry path into a safe relative path.
///
/// Returns `Ok(None)` for the archive root (`.` / `./`).
pub fn sanitize_entry_path(raw: &Path) -> Result<Option<PathBuf>, &'static str> {
    let mut out = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::Normal(part) => {
                if part.to_string_lossy().contains(':') {
                    return Err("':' is not allowed in paths (drive / alternate data stream)");
                }
                out.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir => return Err("path traversal ('..') is not allowed"),
            Component::RootDir | Component::Prefix(_) => return Err("absolute paths are not allowed"),
        }
    }
    Ok((!out.as_os_str().is_empty()).then_some(out))
}

/// A symlink at `rel` pointing to `target` must be relative and resolve
/// (lexically) inside the bundle root.
///
/// `..` is only accepted as a leading component: after a normal component it
/// would be resolved *through* that component, which may itself be a symlink
/// (`d/s -> ..` + `d/t -> s/..` escapes although it looks fine lexically).
pub fn check_symlink_target(rel: &Path, target: &Path) -> Result<(), &'static str> {
    let mut depth: usize = rel.parent().map(|p| p.components().count()).unwrap_or(0);
    let mut descended = false;
    for component in target.components() {
        match component {
            Component::Normal(_) => {
                depth += 1;
                descended = true;
            }
            Component::CurDir => {}
            Component::ParentDir if descended => {
                return Err("'..' after a normal component is not allowed in symlink targets");
            }
            Component::ParentDir => {
                depth = depth
                    .checked_sub(1)
                    .ok_or("symlink target escapes the bundle directory")?;
            }
            Component::RootDir | Component::Prefix(_) => return Err("absolute symlink targets are not allowed"),
        }
    }
    Ok(())
}

/// Reject link creation beneath an existing symlink (prevents chained escapes
/// such as `a -> .` followed by `a/b -> ..`).
fn ensure_no_symlink_ancestors(root: &Path, rel: &Path) -> Result<(), &'static str> {
    let mut current = root.to_path_buf();
    if let Some(parent) = rel.parent() {
        for component in parent.components() {
            current.push(component);
            if let Ok(meta) = fs::symlink_metadata(&current) {
                if meta.file_type().is_symlink() {
                    return Err("entry would be created beneath a symlink");
                }
            }
        }
    }
    Ok(())
}

fn create_symlink(id: &str, root: &Path, rel: &Path, target: &Path, warnings: &mut Vec<String>) -> Result<()> {
    let link = root.join(rel);
    if let Some(dir) = link.parent() {
        fs::create_dir_all(dir).at(dir)?;
    }
    remove_path_if_exists(&link)?;

    let resolved = link.parent().unwrap_or(root).join(target);
    let is_dir = resolved.is_dir();
    let created = if is_dir {
        std::os::windows::fs::symlink_dir(target, &link)
    } else {
        std::os::windows::fs::symlink_file(target, &link)
    };
    if created.is_ok() {
        return Ok(());
    }
    // Creating symlinks needs Developer Mode or admin rights on Windows: copy instead.
    if is_dir {
        // A link to one of its own ancestors (`a/b -> ..`) is a loop: copying it
        // would recurse into the copy until the path is too long.
        let parent = link.parent().unwrap_or(root);
        if let (Ok(src), Ok(parent)) = (fs::canonicalize(&resolved), fs::canonicalize(parent)) {
            if parent.starts_with(&src) {
                return Err(CoreError::UnsafeEntry {
                    id: id.to_string(),
                    entry: rel.display().to_string(),
                    reason: "symlink to an ancestor directory (loop) cannot be replaced by a copy".into(),
                });
            }
        }
        copy_dir_recursive(&resolved, &link)?;
    } else if resolved.is_file() {
        fs::copy(&resolved, &link).at(&link)?;
    } else {
        warnings.push(format!(
            "skipped dangling symlink '{}' -> '{}'",
            rel.display(),
            target.display()
        ));
        return Ok(());
    }
    warnings.push(format!(
        "symlink '{}' -> '{}' replaced by a copy (symlinks unavailable)",
        rel.display(),
        target.display()
    ));
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).at(dst)?;
    for entry in fs::read_dir(src).at(src)? {
        let entry = entry.at(src)?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type().at(&from)?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            fs::copy(&from, &to).at(&to)?;
        }
    }
    Ok(())
}

/// Move `tmp` onto `dest`. If `dest` exists it is first renamed to `.old-<id>`
/// and restored if the swap fails.
fn commit_dir(tmp: &Path, dest: &Path, id: &str) -> Result<()> {
    if fs::symlink_metadata(dest).is_err() {
        return rename_with_retry(tmp, dest).at(dest);
    }
    let old = dest.with_file_name(format!(".old-{id}"));
    remove_path_if_exists(&old)?;
    rename_with_retry(dest, &old).at(dest)?;
    if let Err(e) = rename_with_retry(tmp, dest) {
        let _ = rename_with_retry(&old, dest);
        return Err(e).at(dest);
    }
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

/// Directory renames on Windows can fail transiently while antivirus or the
/// indexer holds a handle on a freshly written file.
fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if attempt < 10 && e.kind() == io::ErrorKind::PermissionDenied => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(50 * attempt));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Remove a file, symlink or directory tree if it exists.
pub fn remove_path_if_exists(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path).at(path),
        Ok(_) => fs::remove_file(path).or_else(|_| fs::remove_dir(path)).at(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).at(path),
    }
}

/// Pack the contents of `src_dir` into a deterministic `.tar.zst` at `out`.
///
/// Used by tests and by bundle-building tooling. Entries are sorted, symlinks
/// are stored as symlinks, ownership and timestamps are normalised.
pub fn pack_dir_tar_zst(src_dir: &Path, out: &Path, level: i32) -> Result<ArchiveInfo> {
    if let Some(dir) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(dir).at(dir)?;
    }
    // Write next to the destination and rename at the end: an interrupted
    // pack never leaves a truncated archive under the final name.
    let mut tmp_name = out.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    tmp_name.push(".tmp");
    let tmp = out.with_file_name(tmp_name);
    let written = (|| -> Result<()> {
        let file = File::create(&tmp).at(&tmp)?;
        let encoder = zstd::stream::write::Encoder::new(BufWriter::new(file), level).at(&tmp)?;
        let mut builder = tar::Builder::new(encoder);
        builder.mode(tar::HeaderMode::Deterministic);
        builder.follow_symlinks(false);
        append_tree(&mut builder, src_dir, Path::new(""))?;
        let encoder = builder.into_inner().at(&tmp)?;
        let writer = encoder.finish().at(&tmp)?;
        writer
            .into_inner()
            .map_err(|e| e.into_error())
            .at(&tmp)?
            .sync_all()
            .at(&tmp)?;
        rename_with_retry(&tmp, out).at(out)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    Ok(ArchiveInfo {
        sha256: sha256_file(out)?,
        size_bytes: fs::metadata(out).at(out)?.len(),
    })
}

fn append_tree<W: io::Write>(builder: &mut tar::Builder<W>, root: &Path, rel: &Path) -> Result<()> {
    let dir = root.join(rel);
    let mut entries: Vec<_> = fs::read_dir(&dir).at(&dir)?.collect::<io::Result<Vec<_>>>().at(&dir)?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = rel.join(entry.file_name());
        let file_type = entry.file_type().at(&path)?;
        if file_type.is_dir() {
            builder.append_dir(&name, &path).at(&path)?;
            append_tree(builder, root, &name)?;
        } else {
            builder.append_path_with_name(&path, &name).at(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rejects_dangerous_paths() {
        assert_eq!(sanitize_entry_path(Path::new("./")).unwrap(), None);
        assert_eq!(
            sanitize_entry_path(Path::new("./bin/clang")).unwrap(),
            Some(Path::new("bin").join("clang"))
        );
        assert!(sanitize_entry_path(Path::new("../evil.txt")).is_err());
        assert!(sanitize_entry_path(Path::new("bin/../../evil.txt")).is_err());
        assert!(sanitize_entry_path(Path::new("/etc/passwd")).is_err());
        assert!(sanitize_entry_path(Path::new(r"C:\Windows\evil.dll")).is_err());
        assert!(sanitize_entry_path(Path::new(r"..\evil.txt")).is_err());
        assert!(sanitize_entry_path(Path::new("file.txt:stream")).is_err());
    }

    #[test]
    fn symlink_targets_must_stay_inside() {
        let rel = Path::new("bin/clang++");
        assert!(check_symlink_target(rel, Path::new("clang")).is_ok());
        assert!(check_symlink_target(rel, Path::new("../lib/libLLVM.so")).is_ok());
        assert!(check_symlink_target(rel, Path::new("../../outside")).is_err());
        assert!(check_symlink_target(Path::new("top"), Path::new("..")).is_err());
        assert!(check_symlink_target(rel, Path::new("/usr/bin/clang")).is_err());
        assert!(check_symlink_target(rel, Path::new("./clang")).is_ok());
        // `s/..` is resolved through `s`, which may itself be a symlink.
        assert!(check_symlink_target(Path::new("d/t"), Path::new("s/..")).is_err());
        assert!(check_symlink_target(rel, Path::new("../lib/../bin/clang")).is_err());
    }
}
