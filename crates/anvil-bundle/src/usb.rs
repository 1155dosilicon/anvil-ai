//! USB drive initialization, bundle packing, upstream sync, and SHA-256 verification.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anvil_agents::skills::embedded as embedded_skills;
use anvil_agents::templates::USB_TEMPLATES;
use anvil_core::hash::sha256_file;
use anvil_core::{pack_dir_tar_zst, BundleSpec, Manifest, Platform, ANVIL_VERSION, MANIFEST_FILE, SCHEMA_VERSION};
use anvil_env::MANAGED_TOOLS;
use serde::Serialize;

use crate::error::{BundleError, Result};
use crate::recipe::{BundleRecipe, RecipeBook, UpstreamArtifact, UpstreamLayout};

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
    pub manifest_path: PathBuf,
}

/// Populate the USB drive root with templates (`AGENTS.md`, `START_HERE.md`,
/// `install.ps1`, `install.sh`), canonical `skills/*.md`, platform binaries
/// (optional), and an initial `manifest.json` if absent.
pub fn init_usb(usb_dir: &Path, win_exe: Option<&Path>, linux_bin: Option<&Path>) -> Result<InitUsbReport> {
    fs::create_dir_all(usb_dir).map_err(io_err(usb_dir))?;
    for plat in [Platform::WindowsX86_64, Platform::LinuxX86_64] {
        let dir = usb_dir.join("bundles").join(plat.as_str());
        fs::create_dir_all(&dir).map_err(io_err(&dir))?;
    }

    let mut templates_written = Vec::new();
    for tpl in USB_TEMPLATES {
        let dst = usb_dir.join(tpl.file_name);
        fs::write(&dst, tpl.contents.as_bytes()).map_err(io_err(&dst))?;
        #[cfg(unix)]
        if tpl.executable {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&dst).map_err(io_err(&dst))?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&dst, perms).map_err(io_err(&dst))?;
        }
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
    if let Some(src) = linux_bin {
        let dst = usb_dir.join("anvil");
        fs::copy(src, &dst).map_err(io_err(&dst))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&dst).map_err(io_err(&dst))?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&dst, perms).map_err(io_err(&dst))?;
        }
        binaries_copied.push("anvil".into());
    }

    let manifest_path = usb_dir.join(MANIFEST_FILE);
    if !manifest_path.is_file() {
        let empty = Manifest {
            schema_version: SCHEMA_VERSION,
            anvil_version: ANVIL_VERSION.to_string(),
            bundles: Vec::new(),
        };
        save_manifest(&manifest_path, &empty)?;
    }

    Ok(InitUsbReport {
        usb_dir: usb_dir.to_path_buf(),
        templates_written,
        skills_written,
        binaries_copied,
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
        Manifest {
            schema_version: SCHEMA_VERSION,
            anvil_version: ANVIL_VERSION.to_string(),
            bundles: Vec::new(),
        }
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
    let suffix = match platform {
        Platform::WindowsX86_64 => ".exe",
        Platform::LinuxX86_64 => "",
    };
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
    if manifest.bundles.iter().any(|b| b.platform == Platform::LinuxX86_64) && !usb_dir.join("anvil").is_file() {
        missing_binaries.push("anvil".into());
    }

    Ok(VerifyUsbReport {
        ok: errors.is_empty(),
        usb_dir: usb_dir.to_path_buf(),
        manifest_path,
        total_bytes,
        bundles,
        missing_templates,
        missing_binaries,
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

    init_usb(&opts.usb_dir, None, None)?;
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
    let stage = tempfile::Builder::new()
        .prefix(&format!("anvil-sync-{}-", recipe.id))
        .tempdir()
        .map_err(io_err(Path::new("tempdir")))?;

    for artifact in &recipe.artifacts {
        let cached = opts.cache_dir.join(&artifact.filename);
        download_artifact(artifact, &cached)?;
        extract_upstream(&cached, artifact.layout, stage.path())?;
    }

    pack_bundle(&PackRequest {
        usb_dir: &opts.usb_dir,
        platform: recipe.platform,
        id: &recipe.id,
        version: &recipe.version,
        source_dir: stage.path(),
        target_subdir: Some(&recipe.target_subdir),
        archive_name: Some(&recipe.archive_name),
        level: opts.level,
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

fn extract_upstream(archive: &Path, layout: UpstreamLayout, stage_root: &Path) -> Result<()> {
    let unpack = tempfile::Builder::new()
        .prefix("anvil-unpack-")
        .tempdir()
        .map_err(io_err(Path::new("tempdir")))?;

    let status = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(unpack.path())
        .status()
        .map_err(|_| BundleError::MissingHostTool("tar".into()))?;
    if !status.success() {
        return Err(BundleError::ExternalCommand {
            command: format!("tar -xf {}", archive.display()),
            detail: format!("exit status {status}"),
        });
    }

    match layout {
        UpstreamLayout::StripTopDir => {
            let src = single_child_dir(unpack.path()).unwrap_or_else(|| unpack.path().to_path_buf());
            merge_tree(&src, stage_root)?;
        }
        UpstreamLayout::IntoBin => {
            let bin = stage_root.join("bin");
            fs::create_dir_all(&bin).map_err(io_err(&bin))?;
            merge_tree(unpack.path(), &bin)?;
        }
        UpstreamLayout::RustComponent => {
            let pkg_root = single_child_dir(unpack.path()).unwrap_or_else(|| unpack.path().to_path_buf());
            // Official Rust tarballs contain a `components` file listing subdir names (e.g. `rustc`, `rust-std-...`).
            let comps_file = pkg_root.join("components");
            if comps_file.is_file() {
                let text = fs::read_to_string(&comps_file).map_err(io_err(&comps_file))?;
                for comp in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    let comp_dir = pkg_root.join(comp);
                    if comp_dir.is_dir() {
                        merge_tree_excluding_meta(&comp_dir, stage_root)?;
                    }
                }
            } else {
                merge_tree_excluding_meta(&pkg_root, stage_root)?;
            }
        }
    }
    Ok(())
}

fn single_child_dir(dir: &Path) -> Option<PathBuf> {
    let mut entries = fs::read_dir(dir).ok()?.filter_map(std::result::Result::ok);
    let first = entries.next()?;
    if entries.next().is_some() {
        return None;
    }
    first.file_type().ok()?.is_dir().then(|| first.path())
}

fn merge_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(io_err(dst))?;
    for entry in fs::read_dir(src).map_err(io_err(src))? {
        let entry = entry.map_err(io_err(src))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type().map_err(io_err(&from))?;
        if ft.is_dir() {
            merge_tree(&from, &to)?;
        } else if ft.is_symlink() {
            #[cfg(unix)]
            {
                let target = fs::read_link(&from).map_err(io_err(&from))?;
                let _ = fs::remove_file(&to);
                std::os::unix::fs::symlink(&target, &to).map_err(io_err(&to))?;
            }
            #[cfg(not(unix))]
            {
                fs::copy(&from, &to).map_err(io_err(&to))?;
            }
        } else {
            fs::copy(&from, &to).map_err(io_err(&to))?;
        }
    }
    Ok(())
}

fn merge_tree_excluding_meta(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(io_err(dst))?;
    for entry in fs::read_dir(src).map_err(io_err(src))? {
        let entry = entry.map_err(io_err(src))?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if matches!(
            name_str.as_ref(),
            "manifest.in" | "install.sh" | "components" | "rust-installer-version"
        ) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if entry.file_type().map_err(io_err(&from))?.is_dir() {
            merge_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(io_err(&to))?;
        }
    }
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

        let init = init_usb(&usb, Some(&fake_exe), Some(&fake_exe)).unwrap();
        assert_eq!(init.templates_written.len(), 4);
        assert_eq!(init.skills_written.len(), 5);
        assert_eq!(init.binaries_copied.len(), 2);
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

    #[test]
    fn sync_dry_run_plans_requested_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let book = RecipeBook::builtin();
        let report = sync_usb(
            &book,
            &SyncOptions {
                usb_dir: tmp.path().join("usb"),
                targets: vec![Platform::WindowsX86_64, Platform::LinuxX86_64],
                only: vec!["rust".into()],
                cache_dir: tmp.path().join("cache"),
                level: 10,
                dry_run: true,
            },
        )
        .unwrap();
        assert!(report.ok && report.dry_run);
        assert_eq!(report.items.len(), 2);
        assert!(report.items.iter().all(|i| i.id == "rust" && i.status == "planned"));
    }
}
