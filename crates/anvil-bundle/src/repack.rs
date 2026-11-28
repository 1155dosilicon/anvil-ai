//! Stream-repack upstream `.tar.xz`, `.tar.gz`, and `.zip` archives directly
//! into `.tar.zst` without extracting onto the host filesystem.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read};
use std::path::{Component, Path, PathBuf};

use anvil_core::hash::sha256_file;
use anvil_core::Platform;
use anvil_env::MANAGED_TOOLS;
use flate2::read::GzDecoder;
use xz2::read::XzDecoder;
use zip::ZipArchive;

use crate::error::{BundleError, Result};
use crate::recipe::UpstreamLayout;

pub struct RepackResult {
    pub sha256: String,
    pub size_bytes: u64,
    pub tools: Vec<String>,
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> BundleError + '_ {
    move |source| BundleError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Stream a list of `(cached_archive_path, layout)` directly into a single `.tar.zst` at `out_path`.
pub fn stream_repack_to_tar_zst(
    inputs: &[(PathBuf, UpstreamLayout)],
    platform: Platform,
    out_path: &Path,
    level: i32,
) -> Result<RepackResult> {
    if let Some(dir) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(io_err(dir))?;
    }
    let tmp_out = out_path.with_extension("tar.zst.tmp");
    let result = repack_into(inputs, platform, &tmp_out, out_path, level);
    if result.is_err() {
        // Never leave a half-written archive behind.
        let _ = fs::remove_file(&tmp_out);
    }
    result
}

fn repack_into(
    inputs: &[(PathBuf, UpstreamLayout)],
    platform: Platform,
    tmp_out: &Path,
    out_path: &Path,
    level: i32,
) -> Result<RepackResult> {
    let tmp_out = tmp_out.to_path_buf();
    let file = File::create(&tmp_out).map_err(io_err(&tmp_out))?;
    let encoder = zstd::stream::write::Encoder::new(BufWriter::with_capacity(1 << 20, file), level.clamp(1, 19))
        .map_err(io_err(&tmp_out))?;
    let mut builder = tar::Builder::new(encoder);
    builder.mode(tar::HeaderMode::Deterministic);

    let mut seen_paths: HashSet<String> = HashSet::new();
    let mut found_tools: HashSet<String> = HashSet::new();
    let exe_suffix = platform.exe_suffix();

    for (archive_path, layout) in inputs {
        let name_lower = archive_path
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();

        if name_lower.ends_with(".zip") {
            append_zip(
                archive_path,
                *layout,
                &mut builder,
                &mut seen_paths,
                &mut found_tools,
                exe_suffix,
            )?;
        } else if name_lower.ends_with(".tar.xz") || name_lower.ends_with(".txz") {
            let f = File::open(archive_path).map_err(io_err(archive_path))?;
            let dec = XzDecoder::new(BufReader::with_capacity(1 << 20, f));
            append_tar(
                archive_path,
                dec,
                *layout,
                &mut builder,
                &mut seen_paths,
                &mut found_tools,
                exe_suffix,
            )?;
        } else if name_lower.ends_with(".tar.gz") || name_lower.ends_with(".tgz") {
            let f = File::open(archive_path).map_err(io_err(archive_path))?;
            let dec = GzDecoder::new(BufReader::with_capacity(1 << 20, f));
            append_tar(
                archive_path,
                dec,
                *layout,
                &mut builder,
                &mut seen_paths,
                &mut found_tools,
                exe_suffix,
            )?;
        } else {
            return Err(BundleError::InvalidRecipe(format!(
                "unsupported upstream archive format: {}",
                archive_path.display()
            )));
        }
    }

    let encoder = builder.into_inner().map_err(io_err(&tmp_out))?;
    let writer = encoder.finish().map_err(io_err(&tmp_out))?;
    writer
        .into_inner()
        .map_err(|e| io_err(&tmp_out)(e.into_error()))?
        .sync_all()
        .map_err(io_err(&tmp_out))?;

    if out_path.exists() {
        let _ = fs::remove_file(out_path);
    }
    fs::rename(&tmp_out, out_path).map_err(io_err(out_path))?;

    let sha256 = sha256_file(out_path)?;
    let size_bytes = fs::metadata(out_path).map_err(io_err(out_path))?.len();
    let mut tools: Vec<String> = MANAGED_TOOLS
        .iter()
        .filter(|t| found_tools.contains(t.name))
        .map(|t| t.name.to_string())
        .collect();
    tools.sort();

    Ok(RepackResult {
        sha256,
        size_bytes,
        tools,
    })
}

fn append_tar<R: Read, W: io::Write>(
    src_path: &Path,
    reader: R,
    layout: UpstreamLayout,
    builder: &mut tar::Builder<W>,
    seen: &mut HashSet<String>,
    found_tools: &mut HashSet<String>,
    exe_suffix: &str,
) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries().map_err(io_err(src_path))? {
        let mut entry = entry.map_err(io_err(src_path))?;
        let etype = entry.header().entry_type();
        if matches!(
            etype,
            tar::EntryType::XGlobalHeader
                | tar::EntryType::XHeader
                | tar::EntryType::GNULongName
                | tar::EntryType::GNULongLink
        ) {
            continue;
        }
        let raw_path = entry.path().map_err(io_err(src_path))?.into_owned();
        let Some(mapped) = map_entry_path(&raw_path, layout) else {
            continue;
        };
        let key = path_key(&mapped);
        if etype == tar::EntryType::Directory {
            if !seen.insert(key) {
                continue;
            }
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(entry.header().mode().unwrap_or(0o755));
            header.set_mtime(entry.header().mtime().unwrap_or(0));
            header.set_cksum();
            builder
                .append_data(&mut header, &mapped, io::empty())
                .map_err(io_err(src_path))?;
            continue;
        }

        if !seen.insert(key) {
            continue;
        }
        record_tool(&mapped, exe_suffix, found_tools);

        match etype {
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(entry.size());
                header.set_mode(entry.header().mode().unwrap_or(0o644));
                header.set_mtime(entry.header().mtime().unwrap_or(0));
                header.set_cksum();
                builder
                    .append_data(&mut header, &mapped, &mut entry)
                    .map_err(io_err(src_path))?;
            }
            tar::EntryType::Symlink => {
                let Some(link) = entry.link_name().map_err(io_err(src_path))?.map(|c| c.into_owned()) else {
                    continue;
                };
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header.set_mode(0o777);
                header.set_mtime(entry.header().mtime().unwrap_or(0));
                builder
                    .append_link(&mut header, &mapped, &link)
                    .map_err(io_err(src_path))?;
            }
            tar::EntryType::Link => {
                let Some(link) = entry.link_name().map_err(io_err(src_path))?.map(|c| c.into_owned()) else {
                    continue;
                };
                let Some(mapped_target) = map_entry_path(&link, layout) else {
                    continue;
                };
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Link);
                header.set_size(0);
                header.set_mode(entry.header().mode().unwrap_or(0o644));
                header.set_mtime(entry.header().mtime().unwrap_or(0));
                builder
                    .append_link(&mut header, &mapped, &mapped_target)
                    .map_err(io_err(src_path))?;
            }
            tar::EntryType::GNUSparse => {
                // Silently dropping it would ship a bundle with a missing file.
                return Err(BundleError::InvalidRecipe(format!(
                    "{}: sparse entry '{}' is not supported",
                    src_path.display(),
                    raw_path.display()
                )));
            }
            // Devices / FIFOs have no place in a toolchain bundle.
            _ => {}
        }
    }
    Ok(())
}

fn append_zip<W: io::Write>(
    src_path: &Path,
    layout: UpstreamLayout,
    builder: &mut tar::Builder<W>,
    seen: &mut HashSet<String>,
    found_tools: &mut HashSet<String>,
    exe_suffix: &str,
) -> Result<()> {
    let file = File::open(src_path).map_err(io_err(src_path))?;
    let mut zip = ZipArchive::new(BufReader::new(file)).map_err(|e| BundleError::ExternalCommand {
        command: format!("zip open {}", src_path.display()),
        detail: e.to_string(),
    })?;

    for idx in 0..zip.len() {
        let mut entry = zip.by_index(idx).map_err(|e| BundleError::ExternalCommand {
            command: format!("zip entry #{idx}"),
            detail: e.to_string(),
        })?;
        let Some(raw_path) = entry.enclosed_name() else {
            continue;
        };
        let Some(mapped) = map_entry_path(&raw_path, layout) else {
            continue;
        };
        let key = path_key(&mapped);
        if entry.is_dir() {
            if !seen.insert(key) {
                continue;
            }
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, &mapped, io::empty())
                .map_err(io_err(src_path))?;
            continue;
        }
        if !seen.insert(key) {
            continue;
        }
        record_tool(&mapped, exe_suffix, found_tools);

        let mode = entry
            .unix_mode()
            .map(|m| m & 0o777)
            .filter(|&m| m != 0)
            .unwrap_or(0o644);

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(entry.size());
        header.set_mode(mode);
        header.set_cksum();
        builder
            .append_data(&mut header, &mapped, &mut entry)
            .map_err(io_err(src_path))?;
    }
    Ok(())
}

/// Rewrite an upstream archive path according to `layout`.
pub fn map_entry_path(raw: &Path, layout: UpstreamLayout) -> Option<PathBuf> {
    let parts: Vec<String> = raw
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        return None;
    }

    match layout {
        UpstreamLayout::StripTopDir => {
            if parts.len() <= 1 {
                return None;
            }
            Some(parts[1..].iter().collect())
        }
        UpstreamLayout::IntoBin => {
            let mut out = PathBuf::from("bin");
            for p in &parts {
                out.push(p);
            }
            Some(out)
        }
        UpstreamLayout::RustComponent => {
            // Official Rust tarballs have layout:
            // `<pkg>-<ver>-<triple>/<component-dir>/{bin,lib,share,etc}/...`
            // Plus top-level metadata files (`install.sh` of the Rust installer, `components`, `rust-installer-version`)
            // and per-component `manifest.in` that we skip.
            if parts.len() <= 2 {
                return None;
            }
            if parts[2] == "manifest.in" {
                return None;
            }
            Some(parts[2..].iter().collect())
        }
    }
}

fn path_key(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn record_tool(mapped: &Path, exe_suffix: &str, found: &mut HashSet<String>) {
    let mut comps = mapped.components();
    let Some(Component::Normal(first)) = comps.next() else {
        return;
    };
    if first != "bin" {
        return;
    }
    let Some(Component::Normal(file)) = comps.next() else {
        return;
    };
    if comps.next().is_some() {
        return;
    }
    let file_str = file.to_string_lossy();
    for tool in MANAGED_TOOLS {
        let expected = format!("{}{exe_suffix}", tool.name);
        if file_str.eq_ignore_ascii_case(&expected) {
            found.insert(tool.name.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_all_upstream_layouts() {
        assert_eq!(
            map_entry_path(Path::new("llvm-mingw-2025/bin/clang.exe"), UpstreamLayout::StripTopDir),
            Some(PathBuf::from("bin/clang.exe"))
        );
        assert_eq!(
            map_entry_path(Path::new("ninja.exe"), UpstreamLayout::IntoBin),
            Some(PathBuf::from("bin/ninja.exe"))
        );
        assert_eq!(
            map_entry_path(
                Path::new("rustc-1.85.1-x86_64-pc-windows-gnu/rustc/bin/rustc.exe"),
                UpstreamLayout::RustComponent
            ),
            Some(PathBuf::from("bin/rustc.exe"))
        );
        assert_eq!(
            map_entry_path(
                Path::new("rustc-1.85.1-x86_64-pc-windows-gnu/rustc/manifest.in"),
                UpstreamLayout::RustComponent
            ),
            None
        );
        assert_eq!(
            map_entry_path(
                Path::new("rustc-1.85.1-x86_64-pc-windows-gnu/install.sh"),
                UpstreamLayout::RustComponent
            ),
            None
        );
    }
}
