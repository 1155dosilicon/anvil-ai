//! Integration tests for the streaming `.tar.zst` extractor.

use std::fs;
use std::path::{Path, PathBuf};

use anvil_core::archive::{extract_bundle_streaming, pack_dir_tar_zst, ExtractRequest};
use anvil_core::hash::{sha256_bytes, sha256_file};
use anvil_core::{CoreError, Progress, SilentProgress};

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// Builds `<tmp>/src` with a small tree and packs it to `<tmp>/bundle.tar.zst`.
fn sample_bundle(tmp: &Path) -> (PathBuf, String, u64) {
    let src = tmp.join("src");
    write(&src.join("bin/tool.txt"), "tool v1");
    write(&src.join("lib/deep/nested/data.bin"), &"x".repeat(200_000));
    fs::create_dir_all(src.join("share/empty")).unwrap();
    let archive = tmp.join("bundle.tar.zst");
    let info = pack_dir_tar_zst(&src, &archive, 3).unwrap();
    (archive, info.sha256, info.size_bytes)
}

fn entries(dir: &Path) -> Vec<String> {
    match fs::read_dir(dir) {
        Ok(rd) => {
            let mut v: Vec<String> = rd
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        }
        Err(_) => Vec::new(),
    }
}

#[test]
fn extracts_and_verifies_sha256_in_one_pass() {
    let tmp = tempfile::tempdir().unwrap();
    let (archive, sha, size) = sample_bundle(tmp.path());
    let dest = tmp.path().join("home/toolchains/sample");

    let report = extract_bundle_streaming(
        &ExtractRequest {
            id: "sample",
            archive: &archive,
            expected_sha256: &sha.to_uppercase(), // comparison is case-insensitive
            expected_size: Some(size),
            dest: &dest,
        },
        &mut SilentProgress,
    )
    .unwrap();

    assert_eq!(report.sha256, sha);
    assert_eq!(report.archive_bytes, size);
    assert_eq!(report.files, 2);
    assert_eq!(fs::read_to_string(dest.join("bin/tool.txt")).unwrap(), "tool v1");
    assert_eq!(fs::read(dest.join("lib/deep/nested/data.bin")).unwrap().len(), 200_000);
    assert!(dest.join("share/empty").is_dir());
    // Only the final directory remains: no .tmp-* / .old-* leftovers.
    assert_eq!(entries(&tmp.path().join("home/toolchains")), vec!["sample"]);
}

#[test]
fn tampered_hash_leaves_destination_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let (archive, _sha, size) = sample_bundle(tmp.path());
    let dest = tmp.path().join("home/toolchains/sample");
    let wrong = "0".repeat(64);

    let err = extract_bundle_streaming(
        &ExtractRequest {
            id: "sample",
            archive: &archive,
            expected_sha256: &wrong,
            expected_size: Some(size),
            dest: &dest,
        },
        &mut SilentProgress,
    )
    .unwrap_err();

    assert!(matches!(err, CoreError::ChecksumMismatch { .. }), "{err}");
    assert!(err.to_string().contains("SHA-256 mismatch"));
    assert!(!dest.exists());
    assert!(entries(&tmp.path().join("home/toolchains")).is_empty());
}

#[test]
fn tampered_archive_keeps_previous_install_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let (archive, sha, _size) = sample_bundle(tmp.path());
    let dest = tmp.path().join("home/toolchains/sample");
    write(&dest.join("previous.txt"), "old install");

    // Flip one byte near the end of the archive: decompression may or may not
    // notice, but the SHA-256 always does.
    let mut bytes = fs::read(&archive).unwrap();
    let idx = bytes.len() - 5;
    bytes[idx] ^= 0xff;
    fs::write(&archive, &bytes).unwrap();

    let err = extract_bundle_streaming(
        &ExtractRequest {
            id: "sample",
            archive: &archive,
            expected_sha256: &sha,
            expected_size: None,
            dest: &dest,
        },
        &mut SilentProgress,
    )
    .unwrap_err();
    assert!(
        matches!(err, CoreError::ChecksumMismatch { .. } | CoreError::Archive { .. }),
        "{err}"
    );
    assert_eq!(fs::read_to_string(dest.join("previous.txt")).unwrap(), "old install");
    assert!(!dest.join("bin").exists());
    assert_eq!(entries(&tmp.path().join("home/toolchains")), vec!["sample"]);
}

#[test]
fn reinstall_replaces_existing_destination_atomically() {
    let tmp = tempfile::tempdir().unwrap();
    let (archive, sha, size) = sample_bundle(tmp.path());
    let dest = tmp.path().join("home/toolchains/sample");
    write(&dest.join("stale.txt"), "stale");

    extract_bundle_streaming(
        &ExtractRequest {
            id: "sample",
            archive: &archive,
            expected_sha256: &sha,
            expected_size: Some(size),
            dest: &dest,
        },
        &mut SilentProgress,
    )
    .unwrap();
    assert!(!dest.join("stale.txt").exists());
    assert!(dest.join("bin/tool.txt").is_file());
    assert_eq!(entries(&tmp.path().join("home/toolchains")), vec!["sample"]);
}

#[test]
fn size_mismatch_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let (archive, sha, size) = sample_bundle(tmp.path());
    let dest = tmp.path().join("home/toolchains/sample");
    let err = extract_bundle_streaming(
        &ExtractRequest {
            id: "sample",
            archive: &archive,
            expected_sha256: &sha,
            expected_size: Some(size + 1),
            dest: &dest,
        },
        &mut SilentProgress,
    )
    .unwrap_err();
    assert!(matches!(err, CoreError::SizeMismatch { .. }), "{err}");
    assert!(!dest.exists());
}

#[test]
fn truncated_archive_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let (archive, _sha, _size) = sample_bundle(tmp.path());
    let bytes = fs::read(&archive).unwrap();
    let truncated = tmp.path().join("truncated.tar.zst");
    fs::write(&truncated, &bytes[..bytes.len() / 2]).unwrap();
    let sha = sha256_file(&truncated).unwrap();
    let dest = tmp.path().join("home/toolchains/sample");
    let err = extract_bundle_streaming(
        &ExtractRequest {
            id: "sample",
            archive: &truncated,
            expected_sha256: &sha,
            expected_size: None,
            dest: &dest,
        },
        &mut SilentProgress,
    )
    .unwrap_err();
    assert!(matches!(err, CoreError::Archive { .. }), "{err}");
    assert!(entries(&tmp.path().join("home/toolchains")).is_empty());
}

#[derive(Default)]
struct Recorder {
    begins: Vec<(String, u64)>,
    last: u64,
    advances: usize,
    finished: Vec<String>,
}

impl Progress for Recorder {
    fn begin(&mut self, label: &str, total: u64) {
        self.begins.push((label.to_string(), total));
    }
    fn advance(&mut self, done: u64) {
        assert!(done >= self.last, "progress must be monotonic");
        self.last = done;
        self.advances += 1;
    }
    fn finish(&mut self, label: &str) {
        self.finished.push(label.to_string());
    }
}

#[test]
fn reports_progress_up_to_archive_size() {
    let tmp = tempfile::tempdir().unwrap();
    let (archive, sha, size) = sample_bundle(tmp.path());
    let mut rec = Recorder::default();
    extract_bundle_streaming(
        &ExtractRequest {
            id: "sample",
            archive: &archive,
            expected_sha256: &sha,
            expected_size: Some(size),
            dest: &tmp.path().join("out/sample"),
        },
        &mut rec,
    )
    .unwrap();
    assert_eq!(rec.begins, vec![("sample".to_string(), size)]);
    assert_eq!(rec.last, size);
    assert!(rec.advances >= 1);
    assert_eq!(rec.finished, vec!["sample"]);
}

// ---------------------------------------------------------------------------
// Hostile archives. The tar crate's high-level builder refuses to write `..`
// paths, so headers are crafted byte by byte.
// ---------------------------------------------------------------------------

enum Raw<'a> {
    File(&'a [u8], &'a [u8]),
    Symlink(&'a [u8], &'a [u8]),
}

fn hostile_tar_zst(path: &Path, items: &[Raw<'_>]) -> String {
    let mut builder = tar::Builder::new(Vec::new());
    for item in items {
        let mut header = tar::Header::new_old();
        let (name, link, data, kind): (&[u8], &[u8], &[u8], tar::EntryType) = match item {
            Raw::File(name, data) => (name, b"", data, tar::EntryType::Regular),
            Raw::Symlink(name, target) => (name, target, b"", tar::EntryType::Symlink),
        };
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.as_old_mut().linkname[..link.len()].copy_from_slice(link);
        header.set_entry_type(kind);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        builder.append(&header, data).unwrap();
    }
    let tar = builder.into_inner().unwrap();
    let compressed = zstd::encode_all(&tar[..], 3).unwrap();
    fs::write(path, &compressed).unwrap();
    sha256_bytes(&compressed)
}

fn extract_hostile(tmp: &Path, items: &[Raw<'_>]) -> (CoreError, PathBuf) {
    let archive = tmp.join("hostile.tar.zst");
    let sha = hostile_tar_zst(&archive, items);
    let dest = tmp.join("home/toolchains/evil");
    let err = extract_bundle_streaming(
        &ExtractRequest {
            id: "evil",
            archive: &archive,
            expected_sha256: &sha,
            expected_size: None,
            dest: &dest,
        },
        &mut SilentProgress,
    )
    .unwrap_err();
    (err, dest)
}

#[test]
fn rejects_parent_dir_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let (err, dest) = extract_hostile(
        tmp.path(),
        &[Raw::File(b"ok.txt", b"fine"), Raw::File(b"../evil.txt", b"pwned")],
    );
    assert!(matches!(err, CoreError::UnsafeEntry { .. }), "{err}");
    assert!(err.to_string().contains(".."), "{err}");
    assert!(!dest.exists());
    assert!(!tmp.path().join("home/toolchains/evil.txt").exists());
    assert!(!tmp.path().join("home/evil.txt").exists());
    assert!(entries(&tmp.path().join("home/toolchains")).is_empty());
}

#[test]
fn rejects_nested_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let (err, _) = extract_hostile(tmp.path(), &[Raw::File(b"bin/../../../evil.txt", b"pwned")]);
    assert!(matches!(err, CoreError::UnsafeEntry { .. }), "{err}");
    assert!(!tmp.path().join("evil.txt").exists());
}

#[test]
fn rejects_absolute_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let (err, dest) = extract_hostile(tmp.path(), &[Raw::File(b"/tmp/anvil-evil-abs.txt", b"pwned")]);
    assert!(matches!(err, CoreError::UnsafeEntry { .. }), "{err}");
    assert!(!dest.exists());
}

#[test]
fn rejects_symlink_escaping_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let (err, dest) = extract_hostile(tmp.path(), &[Raw::Symlink(b"bin/escape", b"../../outside")]);
    assert!(matches!(err, CoreError::UnsafeEntry { .. }), "{err}");
    assert!(!dest.exists());

    let (err, _) = extract_hostile(tmp.path(), &[Raw::Symlink(b"abs", b"/etc/passwd")]);
    assert!(matches!(err, CoreError::UnsafeEntry { .. }), "{err}");
}

#[cfg(unix)]
#[test]
fn keeps_internal_symlinks_and_blocks_chained_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("ok.tar.zst");
    let sha = hostile_tar_zst(
        &archive,
        &[
            Raw::File(b"bin/clang-20", b"#!clang"),
            Raw::Symlink(b"bin/clang", b"clang-20"),
        ],
    );
    let dest = tmp.path().join("home/toolchains/llvm");
    extract_bundle_streaming(
        &ExtractRequest {
            id: "llvm",
            archive: &archive,
            expected_sha256: &sha,
            expected_size: None,
            dest: &dest,
        },
        &mut SilentProgress,
    )
    .unwrap();
    assert_eq!(
        fs::read_link(dest.join("bin/clang")).unwrap(),
        PathBuf::from("clang-20")
    );
    assert_eq!(fs::read_to_string(dest.join("bin/clang")).unwrap(), "#!clang");

    // `a -> .` is harmless on its own, but `a/b -> ..` would then point outside.
    let (err, _) = extract_hostile(
        tmp.path(),
        &[Raw::Symlink(b"x/a", b".."), Raw::Symlink(b"x/a/b", b"..")],
    );
    assert!(matches!(err, CoreError::UnsafeEntry { .. }), "{err}");
}

/// `d/s -> ..` resolves to the root; `d/t -> s/..` looks like `d` lexically
/// but is resolved through the symlink `s`, i.e. *outside* the bundle.
#[test]
fn rejects_escape_through_sibling_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let (err, dest) = extract_hostile(
        tmp.path(),
        &[Raw::Symlink(b"d/s", b".."), Raw::Symlink(b"d/t", b"s/..")],
    );
    assert!(matches!(err, CoreError::UnsafeEntry { .. }), "{err}");
    assert!(!dest.exists());
}

/// A link to another link listed *before* it in the archive must still be
/// created correctly (on Windows the target has to exist first).
#[test]
fn symlink_chains_in_any_archive_order() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("chain.tar.zst");
    let sha = hostile_tar_zst(
        &archive,
        &[
            Raw::Symlink(b"bin/clang++", b"clang"),
            Raw::Symlink(b"bin/clang", b"clang-20"),
            Raw::File(b"bin/clang-20", b"#!clang"),
        ],
    );
    let dest = tmp.path().join("home/toolchains/llvm");
    let report = extract_bundle_streaming(
        &ExtractRequest {
            id: "llvm",
            archive: &archive,
            expected_sha256: &sha,
            expected_size: None,
            dest: &dest,
        },
        &mut SilentProgress,
    )
    .unwrap();
    assert_eq!(report.links, 2);
    assert_eq!(fs::read_to_string(dest.join("bin/clang++")).unwrap(), "#!clang");
    assert_eq!(fs::read_to_string(dest.join("bin/clang")).unwrap(), "#!clang");
    assert!(
        !report.warnings.iter().any(|w| w.contains("dangling")),
        "{:?}",
        report.warnings
    );
}
