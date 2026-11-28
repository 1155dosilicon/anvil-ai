//! USB drive initialization, bundle packing, upstream sync, and SHA-256 verification.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anvil_agents::skills::embedded as embedded_skills;
use anvil_agents::templates::USB_TEMPLATES;
use anvil_core::hash::sha256_file;
use anvil_core::{pack_dir_tar_zst, BundleSpec, Manifest, Platform, ANVIL_VERSION, MANIFEST_FILE};
use anvil_env::MANAGED_TOOLS;
use serde::Serialize;

use crate::error::{BundleError, Result};
use crate::recipe::{BundleRecipe, RecipeBook, UpstreamArtifact};
use crate::repack::stream_repack_to_tar_zst;

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> BundleError + '_ {
    move |source| BundleError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InitUsbReport {
    pub usb_dir: PathBuf,
    pub templates_written: Vec<String>,
    pub skills_written: Vec<String>,
    pub binaries_copied: Vec<String>,
    /// Leftovers of the Linux support dropped after Anvil 0.1 that were removed.
    pub removed_legacy: Vec<String>,
    pub manifest_path: PathBuf,
}

/// Root files written by Anvil 0.1 for Linux hosts.
const LEGACY_FILES: &[&str] = &["install.sh"];
/// Bundle directories of platforms this build no longer supports.
const LEGACY_PLATFORM_DIRS: &[&str] = &["linux-x86_64"];

fn is_elf(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok_and(|()| &magic == b"\x7fELF")
}

/// Leftovers of the dropped Linux support present on `usb_dir` (relative names).
pub fn legacy_leftovers(usb_dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = LEGACY_FILES
        .iter()
        .filter(|f| usb_dir.join(f).is_file())
        .map(|f| f.to_string())
        .collect();
    let linux_bin = usb_dir.join("anvil");
    if linux_bin.is_file() && is_elf(&linux_bin) {
        found.push("anvil".into());
    }
    for dir in LEGACY_PLATFORM_DIRS {
        if usb_dir.join("bundles").join(dir).is_dir() {
            found.push(format!("bundles/{dir}/"));
        }
    }
    found
}

/// Remove [`legacy_leftovers`] and drop unsupported bundles from `manifest.json`.
fn prune_legacy(usb_dir: &Path) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    for rel in legacy_leftovers(usb_dir) {
        let path = usb_dir.join(rel.trim_end_matches('/'));
        if rel.ends_with('/') {
            fs::remove_dir_all(&path).map_err(io_err(&path))?;
        } else {
            fs::remove_file(&path).map_err(io_err(&path))?;
        }
        removed.push(rel);
    }
    let manifest_path = usb_dir.join(MANIFEST_FILE);
    if manifest_path.is_file() {
        // An unreadable manifest is left alone: `verify` reports it.
        if let Ok(mut manifest) = Manifest::load(&manifest_path) {
            if !manifest.unsupported.is_empty() {
                removed.extend(manifest.unsupported.iter().map(|b| format!("{MANIFEST_FILE}: {b}")));
                manifest.anvil_version = ANVIL_VERSION.to_string();
                save_manifest(&manifest_path, &manifest)?;
            }
        }
    }
    Ok(removed)
}

/// Populate the USB drive root with templates (`AGENTS.md`, `START_HERE.md`,
/// `install.ps1`), canonical `skills/*.md`, `anvil.exe` (optional),
/// and an initial `manifest.json` if absent. Leftovers of the Linux support
/// of Anvil 0.1 (`install.sh`, the ELF `anvil`, `bundles/linux-x86_64/` and
/// their manifest entries) are removed.
pub fn init_usb(usb_dir: &Path, win_exe: Option<&Path>) -> Result<InitUsbReport> {
    fs::create_dir_all(usb_dir).map_err(io_err(usb_dir))?;
    let removed_legacy = prune_legacy(usb_dir)?;
    let bundles_dir = usb_dir.join("bundles").join(Platform::WindowsX86_64.as_str());
    fs::create_dir_all(&bundles_dir).map_err(io_err(&bundles_dir))?;

    let mut templates_written = Vec::new();
    for tpl in USB_TEMPLATES {
        let dst = usb_dir.join(tpl.file_name);
        fs::write(&dst, tpl.contents.as_bytes()).map_err(io_err(&dst))?;
        templates_written.push(tpl.file_name.to_string());
    }

    let skills_dir = usb_dir.join("skills");
    fs::create_dir_all(&skills_dir).map_err(io_err(&skills_dir))?;
    let mut skills_written = Vec::new();
    for skill in embedded_skills() {
        let file = format!("{}.md", skill.name);
        let dst = skills_dir.join(&file);
        fs::write(&dst, skill.canonical().as_bytes()).map_err(io_err(&dst))?;
        skills_written.push(file);
    }

    let mut binaries_copied = Vec::new();
    if let Some(src) = win_exe {
        let dst = usb_dir.join("anvil.exe");
        fs::copy(src, &dst).map_err(io_err(&dst))?;
        binaries_copied.push("anvil.exe".into());
    }

    let manifest_path = usb_dir.join(MANIFEST_FILE);
    if !manifest_path.is_file() {
        save_manifest(&manifest_path, &Manifest::new(Vec::new()))?;
    }

    Ok(InitUsbReport {
        usb_dir: usb_dir.to_path_buf(),
        templates_written,
        skills_written,
        binaries_copied,
        removed_legacy,
        manifest_path,
    })
}

#[derive(Debug, Clone)]
pub struct PackRequest<'a> {
    pub usb_dir: &'a Path,
    pub platform: Platform,
    pub id: &'a str,
    pub version: &'a str,
    pub source_dir: &'a Path,
    pub target_subdir: Option<&'a str>,
    pub archive_name: Option<&'a str>,
    pub level: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackReport {
    pub id: String,
    pub platform: Platform,
    pub version: String,
    pub archive: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
    pub tools: Vec<String>,
    pub manifest_path: PathBuf,
}

/// Pack `source_dir` into `<usb>/bundles/<platform>/<archive>.tar.zst` and upsert it in `manifest.json`.
pub fn pack_bundle(req: &PackRequest<'_>) -> Result<PackReport> {
    let archive_file = req
        .archive_name
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}.tar.zst", req.id));
    let rel_archive = format!("bundles/{}/{}", req.platform.as_str(), archive_file);
    let out_path = req
        .usb_dir
        .join("bundles")
        .join(req.platform.as_str())
        .join(&archive_file);
    let target_subdir = req
        .target_subdir
        .map(str::to_string)
        .unwrap_or_else(|| format!("toolchains/{}", req.id));

    let info = pack_dir_tar_zst(req.source_dir, &out_path, req.level.clamp(1, 19))?;
    let tools = detect_tools_in_source(req.source_dir, req.platform);

    let manifest_path = req.usb_dir.join(MANIFEST_FILE);
    let mut manifest = if manifest_path.is_file() {
        Manifest::load(&manifest_path)?
    } else {
        Manifest::new(Vec::new())
    };
    manifest.anvil_version = ANVIL_VERSION.to_string();

    let spec = BundleSpec {
        id: req.id.to_string(),
        platform: req.platform,
        archive: rel_archive,
        sha256: info.sha256.clone(),
        size_bytes: info.size_bytes,
        target_subdir,
        version: req.version.to_string(),
        tools: tools.clone(),
    };
    if let Some(slot) = manifest
        .bundles
        .iter_mut()
        .find(|b| b.id == spec.id && b.platform == spec.platform)
    {
        *slot = spec;
    } else {
        manifest.bundles.push(spec);
    }
    manifest.validate()?;
    save_manifest(&manifest_path, &manifest)?;

    Ok(PackReport {
        id: req.id.to_string(),
        platform: req.platform,
        version: req.version.to_string(),
        archive: out_path,
        sha256: info.sha256,
        size_bytes: info.size_bytes,
        tools,
        manifest_path,
    })
}

fn detect_tools_in_source(source_dir: &Path, platform: Platform) -> Vec<String> {
    let suffix = platform.exe_suffix();
    let bin_dir = source_dir.join("bin");
    let mut found = Vec::new();
    for tool in MANAGED_TOOLS {
        let candidate = bin_dir.join(format!("{}{suffix}", tool.name));
        if candidate.is_file() {
            found.push(tool.name.to_string());
        }
    }
    found
}

#[derive(Debug, Clone, Serialize)]
pub struct BundleVerifyItem {
    pub id: String,
    pub platform: Platform,
    pub version: String,
    pub archive: PathBuf,
    pub ok: bool,
    pub expected_size: u64,
    pub actual_size: Option<u64>,
    pub expected_sha256: String,
    pub actual_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyUsbReport {
    pub ok: bool,
    pub usb_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub total_bytes: u64,
    pub bundles: Vec<BundleVerifyItem>,
    pub missing_templates: Vec<String>,
    pub missing_binaries: Vec<String>,
    /// Non-fatal findings (e.g. leftovers of the dropped Linux support).
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

/// Verify every archive listed in `<usb>/manifest.json` against its exact size and SHA-256.
pub fn verify_usb(usb_dir: &Path) -> Result<VerifyUsbReport> {
    let manifest_path = usb_dir.join(MANIFEST_FILE);
    let manifest = Manifest::load(&manifest_path)?;

    let mut bundles = Vec::new();
    let mut errors = Vec::new();
    let mut total_bytes = 0u64;

    for b in &manifest.bundles {
        let archive = b.archive_path(usb_dir);
        let mut item = BundleVerifyItem {
            id: b.id.clone(),
            platform: b.platform,
            version: b.version.clone(),
            archive: archive.clone(),
            ok: false,
            expected_size: b.size_bytes,
            actual_size: None,
            expected_sha256: b.sha256.clone(),
            actual_sha256: None,
            error: None,
        };
        match fs::metadata(&archive) {
            Err(e) => {
                let msg = format!("missing archive {}: {e}", archive.display());
                item.error = Some(msg.clone());
                errors.push(msg);
            }
            Ok(meta) => {
                let actual_size = meta.len();
                item.actual_size = Some(actual_size);
                total_bytes += actual_size;
                match sha256_file(&archive) {
                    Err(e) => {
                        let msg = format!("failed hashing {}: {e}", archive.display());
                        item.error = Some(msg.clone());
                        errors.push(msg);
                    }
                    Ok(actual_sha) => {
                        item.actual_sha256 = Some(actual_sha.clone());
                        if actual_size != b.size_bytes {
                            let msg = format!(
                                "size mismatch for {} ({}): expected {}, got {}",
                                b.id, b.platform, b.size_bytes, actual_size
                            );
                            item.error = Some(msg.clone());
                            errors.push(msg);
                        } else if !actual_sha.eq_ignore_ascii_case(&b.sha256) {
                            let msg = format!(
                                "SHA-256 mismatch for {} ({}): expected {}, got {}",
                                b.id, b.platform, b.sha256, actual_sha
                            );
                            item.error = Some(msg.clone());
                            errors.push(msg);
                        } else {
                            item.ok = true;
                        }
                    }
                }
            }
        }
        bundles.push(item);
    }

    let missing_templates: Vec<String> = USB_TEMPLATES
        .iter()
        .filter(|t| !usb_dir.join(t.file_name).is_file())
        .map(|t| t.file_name.to_string())
        .collect();

    let mut missing_binaries = Vec::new();
    if manifest.bundles.iter().any(|b| b.platform == Platform::WindowsX86_64) && !usb_dir.join("anvil.exe").is_file() {
        missing_binaries.push("anvil.exe".into());
    }

    let mut warnings: Vec<String> = manifest
        .unsupported
        .iter()
        .map(|b| format!("{MANIFEST_FILE} lists unsupported bundle {b} (ignored)"))
        .collect();
    warnings.extend(
        legacy_leftovers(usb_dir)
            .into_iter()
            .map(|rel| format!("legacy Linux leftover '{rel}'")),
    );
    if !warnings.is_empty() {
        warnings.push(format!(
            "run `anvil.exe bundle init-usb --usb {}` to clean up",
            usb_dir.display()
        ));
    }

    Ok(VerifyUsbReport {
        ok: errors.is_empty(),
        usb_dir: usb_dir.to_path_buf(),
        manifest_path,
        total_bytes,
        bundles,
        missing_templates,
        missing_binaries,
        warnings,
        errors,
    })
}

#[derive(Debug, Clone)]
pub struct SyncOptions {
    pub usb_dir: PathBuf,
    pub targets: Vec<Platform>,
    pub only: Vec<String>,
    pub cache_dir: PathBuf,
    pub level: i32,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncPlanItem {
    pub id: String,
    pub platform: Platform,
    pub version: String,
    pub archive_name: String,
    pub urls: Vec<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncReport {
    pub ok: bool,
    pub dry_run: bool,
    pub usb_dir: PathBuf,
    pub items: Vec<SyncPlanItem>,
}

/// Download upstream toolchains via `curl`, unpack and normalize into a clean staging directory,
/// and pack them into `.tar.zst` bundles on `opts.usb_dir`.
pub fn sync_usb(book: &RecipeBook, opts: &SyncOptions) -> Result<SyncReport> {
    let selected = book.select(&opts.targets, &opts.only);
    if opts.dry_run {
        let items = selected
            .into_iter()
            .map(|r| SyncPlanItem {
                id: r.id.clone(),
                platform: r.platform,
                version: r.version.clone(),
                archive_name: r.archive_name.clone(),
                urls: r.artifacts.iter().map(|a| a.url.clone()).collect(),
                status: "planned".into(),
                sha256: None,
                size_bytes: None,
            })
            .collect();
        return Ok(SyncReport {
            ok: true,
            dry_run: true,
            usb_dir: opts.usb_dir.clone(),
            items,
        });
    }

    init_usb(&opts.usb_dir, None)?;
    fs::create_dir_all(&opts.cache_dir).map_err(io_err(&opts.cache_dir))?;

    let mut items = Vec::new();
    for recipe in selected {
        let packed = sync_one_bundle(recipe, opts)?;
        items.push(SyncPlanItem {
            id: recipe.id.clone(),
            platform: recipe.platform,
            version: recipe.version.clone(),
            archive_name: recipe.archive_name.clone(),
            urls: recipe.artifacts.iter().map(|a| a.url.clone()).collect(),
            status: "synced".into(),
            sha256: Some(packed.sha256),
            size_bytes: Some(packed.size_bytes),
        });
    }

    Ok(SyncReport {
        ok: true,
        dry_run: false,
        usb_dir: opts.usb_dir.clone(),
        items,
    })
}

fn sync_one_bundle(recipe: &BundleRecipe, opts: &SyncOptions) -> Result<PackReport> {
    let mut inputs = Vec::with_capacity(recipe.artifacts.len());
    for artifact in &recipe.artifacts {
        let cached = opts.cache_dir.join(&artifact.filename);
        eprintln!("==> [{}/{}] fetching {}", recipe.platform, recipe.id, artifact.filename);
        download_artifact(artifact, &cached)?;
        inputs.push((cached, artifact.layout));
    }

    let rel_archive = format!("bundles/{}/{}", recipe.platform.as_str(), recipe.archive_name);
    let out_path = opts
        .usb_dir
        .join("bundles")
        .join(recipe.platform.as_str())
        .join(&recipe.archive_name);

    eprintln!(
        "==> [{}/{}] stream-repacking into {} (zstd level {})...",
        recipe.platform,
        recipe.id,
        out_path.display(),
        opts.level
    );
    let repacked = stream_repack_to_tar_zst(&inputs, recipe.platform, &out_path, opts.level)?;

    let manifest_path = opts.usb_dir.join(MANIFEST_FILE);
    let mut manifest = if manifest_path.is_file() {
        Manifest::load(&manifest_path)?
    } else {
        Manifest::new(Vec::new())
    };
    manifest.anvil_version = ANVIL_VERSION.to_string();

    let spec = BundleSpec {
        id: recipe.id.clone(),
        platform: recipe.platform,
        archive: rel_archive,
        sha256: repacked.sha256.clone(),
        size_bytes: repacked.size_bytes,
        target_subdir: recipe.target_subdir.clone(),
        version: recipe.version.clone(),
        tools: repacked.tools.clone(),
    };
    if let Some(slot) = manifest
        .bundles
        .iter_mut()
        .find(|b| b.id == spec.id && b.platform == spec.platform)
    {
        *slot = spec;
    } else {
        manifest.bundles.push(spec);
    }
    manifest.validate()?;
    save_manifest(&manifest_path, &manifest)?;

    Ok(PackReport {
        id: recipe.id.clone(),
        platform: recipe.platform,
        version: recipe.version.clone(),
        archive: out_path,
        sha256: repacked.sha256,
        size_bytes: repacked.size_bytes,
        tools: repacked.tools,
        manifest_path,
    })
}

fn download_artifact(artifact: &UpstreamArtifact, dest: &Path) -> Result<()> {
    if dest.is_file() {
        if let Some(expected) = &artifact.sha256 {
            if sha256_file(dest)?.eq_ignore_ascii_case(expected) {
                return Ok(());
            }
        } else if fs::metadata(dest).is_ok_and(|m| m.len() > 0) {
            return Ok(());
        }
    }
    let tmp = dest.with_extension("part");
    let status = Command::new("curl")
        .args(["--fail", "--location", "--retry", "3", "--continue-at", "-", "-o"])
        .arg(&tmp)
        .arg(&artifact.url)
        .status()
        .map_err(|_| BundleError::MissingHostTool("curl".into()))?;
    if !status.success() {
        return Err(BundleError::ExternalCommand {
            command: format!("curl {}", artifact.url),
            detail: format!("exit status {status}"),
        });
    }
    if let Some(expected) = &artifact.sha256 {
        let actual = sha256_file(&tmp)?;
        if !actual.eq_ignore_ascii_case(expected) {
            let _ = fs::remove_file(&tmp);
            return Err(BundleError::ExternalCommand {
                command: format!("sha256 {}", artifact.filename),
                detail: format!("expected {expected}, got {actual}"),
            });
        }
    }
    fs::rename(&tmp, dest).map_err(io_err(dest))?;
    Ok(())
}

fn save_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    let mut text = serde_json::to_string_pretty(manifest).expect("manifest serializes");
    text.push('\n');
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text.as_bytes()).map_err(io_err(&tmp))?;
    fs::rename(&tmp, path).map_err(io_err(path))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_pack_and_verify_usb_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let usb = tmp.path().join("usb");
        let fake_exe = tmp.path().join("anvil-bin");
        fs::write(&fake_exe, b"fake-binary").unwrap();

        let init = init_usb(&usb, Some(&fake_exe)).unwrap();
        assert_eq!(init.templates_written.len(), 3);
        assert_eq!(init.skills_written.len(), 5);
        assert_eq!(init.binaries_copied, vec!["anvil.exe"]);
        assert!(usb.join("AGENTS.md").is_file());
        assert!(usb.join("skills").join("rust-systems.md").is_file());

        // Create a fake build-tools source directory.
        let src = tmp.path().join("src-build-tools");
        fs::create_dir_all(src.join("bin")).unwrap();
        fs::write(src.join("bin").join("cmake.exe"), b"cmake-bin").unwrap();
        fs::write(src.join("bin").join("ninja.exe"), b"ninja-bin").unwrap();

        let packed = pack_bundle(&PackRequest {
            usb_dir: &usb,
            platform: Platform::WindowsX86_64,
            id: "build-tools",
            version: "3.31.6",
            source_dir: &src,
            target_subdir: None,
            archive_name: None,
            level: 3,
        })
        .unwrap();
        assert_eq!(packed.tools, vec!["cmake", "ninja"]);
        assert!(packed.archive.is_file());

        let verified = verify_usb(&usb).unwrap();
        assert!(verified.ok, "{verified:?}");
        assert_eq!(verified.bundles.len(), 1);
        assert!(verified.missing_templates.is_empty());
        assert!(verified.missing_binaries.is_empty());

        // Tampering with the archive must fail verification.
        let mut bytes = fs::read(&packed.archive).unwrap();
        bytes[0] ^= 0xff;
        fs::write(&packed.archive, bytes).unwrap();
        let bad = verify_usb(&usb).unwrap();
        assert!(!bad.ok);
        assert!(!bad.errors.is_empty());
    }

    /// A drive written by Anvil 0.1 (Windows + Linux) stays usable and is cleaned by `init-usb`.
    #[test]
    fn legacy_linux_drive_is_readable_and_pruned() {
        let tmp = tempfile::tempdir().unwrap();
        let usb = tmp.path().join("usb");
        init_usb(&usb, None).unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(src.join("bin")).unwrap();
        fs::write(src.join("bin").join("cmake.exe"), b"cmake-bin").unwrap();
        pack_bundle(&PackRequest {
            usb_dir: &usb,
            platform: Platform::WindowsX86_64,
            id: "build-tools",
            version: "1",
            source_dir: &src,
            target_subdir: None,
            archive_name: None,
            level: 1,
        })
        .unwrap();

        // Simulate the 0.1 layout: Linux manifest entry, bundles, install.sh and ELF binary.
        let manifest_path = usb.join(MANIFEST_FILE);
        let mut value: serde_json::Value = serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        let mut linux = value["bundles"][0].clone();
        linux["platform"] = "linux-x86_64".into();
        linux["archive"] = "bundles/linux-x86_64/build-tools.tar.zst".into();
        value["bundles"].as_array_mut().unwrap().push(linux);
        fs::write(&manifest_path, value.to_string()).unwrap();
        fs::create_dir_all(usb.join("bundles/linux-x86_64")).unwrap();
        fs::write(usb.join("bundles/linux-x86_64/build-tools.tar.zst"), b"x").unwrap();
        fs::write(usb.join("install.sh"), b"#!/bin/sh\n").unwrap();
        fs::write(usb.join("anvil"), b"\x7fELF\x02\x01").unwrap();

        let verified = verify_usb(&usb).unwrap();
        assert!(verified.ok, "{verified:?}");
        assert_eq!(verified.bundles.len(), 1);
        assert!(verified
            .warnings
            .iter()
            .any(|w| w.contains("build-tools (linux-x86_64)")));
        assert!(verified.warnings.iter().any(|w| w.contains("install.sh")));
        assert!(verified.warnings.iter().any(|w| w.contains("bundles/linux-x86_64/")));

        let init = init_usb(&usb, None).unwrap();
        assert_eq!(
            init.removed_legacy,
            vec![
                "install.sh",
                "anvil",
                "bundles/linux-x86_64/",
                "manifest.json: build-tools (linux-x86_64)"
            ]
        );
        assert!(!usb.join("install.sh").exists() && !usb.join("anvil").exists());
        assert!(!usb.join("bundles/linux-x86_64").exists());
        assert!(!fs::read_to_string(&manifest_path).unwrap().contains("linux"));
        let clean = verify_usb(&usb).unwrap();
        assert!(clean.ok && clean.warnings.is_empty(), "{clean:?}");

        // A non-ELF file named `anvil` is not ours to delete.
        fs::write(usb.join("anvil"), b"notes").unwrap();
        assert!(init_usb(&usb, None).unwrap().removed_legacy.is_empty());
        assert!(usb.join("anvil").is_file());
    }

    #[test]
    fn sync_dry_run_plans_requested_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let book = RecipeBook::builtin();
        let report = sync_usb(
            &book,
            &SyncOptions {
                usb_dir: tmp.path().join("usb"),
                targets: vec![Platform::WindowsX86_64],
                only: vec!["rust".into()],
                cache_dir: tmp.path().join("cache"),
                level: 10,
                dry_run: true,
            },
        )
        .unwrap();
        assert!(report.ok && report.dry_run);
        assert_eq!(report.items.len(), 1);
        assert!(report.items.iter().all(|i| i.id == "rust" && i.status == "planned"));
    }
}
