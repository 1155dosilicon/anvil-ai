//! Persistent user environment (`ANVIL_HOME\bin` on the user's PATH).
//!
//! Modifies `HKCU\Environment\Path` (+ `ANVIL_HOME`), followed by a
//! `WM_SETTINGCHANGE` broadcast so Explorer, new terminals and GUI apps pick
//! it up without logging off. No administrator rights are needed.
//!
//! [`EnvMode::Sandbox`] redirects everything into a JSON file standing in for
//! `HKCU\Environment` so tests never touch the real registry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anvil_core::error::IoContext;
use anvil_core::{CoreError, InstallLayout};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::winpath;

pub use anvil_core::envmode::{EnvMode, ENV_SANDBOX_VAR};

/// What an apply/remove operation looked at and changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EnvChange {
    /// Files / registry values inspected.
    pub targets: Vec<String>,
    /// Subset of `targets` that was actually modified.
    pub modified: Vec<String>,
    /// Whether `WM_SETTINGCHANGE` was broadcast (Windows only).
    pub broadcast: bool,
    pub sandbox: bool,
}

impl EnvChange {
    pub fn changed(&self) -> bool {
        !self.modified.is_empty()
    }
}

/// Add `ANVIL_HOME\bin` to the persistent user PATH. Idempotent.
pub fn apply_user_env(mode: &EnvMode, layout: &InstallLayout) -> Result<EnvChange> {
    let mut change = apply_windows_env(windows_store(mode)?.as_mut(), layout)?;
    change.sandbox = mode.is_sandbox();
    Ok(change)
}

/// Undo [`apply_user_env`]. Idempotent.
pub fn remove_user_env(mode: &EnvMode, layout: &InstallLayout) -> Result<EnvChange> {
    let mut change = remove_windows_env(windows_store(mode)?.as_mut(), layout)?;
    change.sandbox = mode.is_sandbox();
    Ok(change)
}

/// Whether the persistent user environment points to `layout`.
pub fn user_env_configured(mode: &EnvMode, layout: &InstallLayout) -> Result<bool> {
    windows_env_status(windows_store(mode)?.as_ref(), layout)
}

fn windows_store(mode: &EnvMode) -> Result<Box<dyn WinEnvStore>> {
    Ok(match mode {
        EnvMode::Real => Box::new(registry::RegistryEnvStore::open()?),
        EnvMode::Sandbox(root) => Box::new(FileEnvStore::new(root.join(FileEnvStore::FILE_NAME))),
    })
}

/// A string value of `HKCU\Environment`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WinEnvValue {
    pub data: String,
    /// `REG_EXPAND_SZ` when true, `REG_SZ` otherwise.
    pub expand: bool,
}

/// Abstraction over `HKCU\Environment` so the logic is testable without the registry.
pub trait WinEnvStore {
    fn describe(&self, name: &str) -> String;
    fn get(&self, name: &str) -> Result<Option<WinEnvValue>>;
    fn set(&mut self, name: &str, value: &WinEnvValue) -> Result<()>;
    fn delete(&mut self, name: &str) -> Result<()>;
    /// Notify running applications; returns whether a broadcast happened.
    fn notify(&self) -> bool;
}

/// JSON-file backed [`WinEnvStore`] used in sandbox mode and tests.
#[derive(Debug, Clone)]
pub struct FileEnvStore {
    path: PathBuf,
}

impl FileEnvStore {
    pub const FILE_NAME: &'static str = "hkcu-environment.json";

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<BTreeMap<String, WinEnvValue>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text).map_err(|source| {
                CoreError::Json {
                    path: self.path.clone(),
                    source,
                }
                .into()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(e).at(&self.path).map_err(Into::into),
        }
    }

    fn write(&self, map: &BTreeMap<String, WinEnvValue>) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).at(dir)?;
        }
        let json = serde_json::to_string_pretty(map).expect("serializable");
        std::fs::write(&self.path, json).at(&self.path)?;
        Ok(())
    }

    /// Registry value names are case-insensitive.
    fn key<'a>(map: &'a BTreeMap<String, WinEnvValue>, name: &str) -> Option<&'a String> {
        map.keys().find(|k| k.eq_ignore_ascii_case(name))
    }
}

impl WinEnvStore for FileEnvStore {
    fn describe(&self, name: &str) -> String {
        format!("{}#{name}", self.path.display())
    }

    fn get(&self, name: &str) -> Result<Option<WinEnvValue>> {
        let map = self.read()?;
        Ok(Self::key(&map, name).and_then(|k| map.get(k)).cloned())
    }

    fn set(&mut self, name: &str, value: &WinEnvValue) -> Result<()> {
        let mut map = self.read()?;
        let key = Self::key(&map, name).cloned().unwrap_or_else(|| name.to_string());
        map.insert(key, value.clone());
        self.write(&map)
    }

    fn delete(&mut self, name: &str) -> Result<()> {
        let mut map = self.read()?;
        if let Some(key) = Self::key(&map, name).cloned() {
            map.remove(&key);
            self.write(&map)?;
        }
        Ok(())
    }

    fn notify(&self) -> bool {
        false
    }
}

const PATH_VALUE: &str = "Path";
const HOME_VALUE: &str = "ANVIL_HOME";

/// Prepend `ANVIL_HOME\bin` to the user `Path` (preserving its registry type)
/// and set `ANVIL_HOME`. Idempotent; broadcasts only when something changed.
pub fn apply_windows_env(store: &mut dyn WinEnvStore, layout: &InstallLayout) -> Result<EnvChange> {
    let bin = layout.bin_dir().display().to_string();
    let home = layout.root().display().to_string();
    let mut change = EnvChange::default();

    change.targets.push(store.describe(PATH_VALUE));
    let current = store.get(PATH_VALUE)?;
    let edit = winpath::add_entry(current.as_ref().map_or("", |v| v.data.as_str()), &bin);
    if edit.changed {
        // A missing Path is created as REG_EXPAND_SZ, like Windows itself does.
        let expand = current.as_ref().is_none_or(|v| v.expand);
        store.set(
            PATH_VALUE,
            &WinEnvValue {
                data: edit.value,
                expand,
            },
        )?;
        change.modified.push(store.describe(PATH_VALUE));
    }

    change.targets.push(store.describe(HOME_VALUE));
    if store.get(HOME_VALUE)?.is_none_or(|v| v.data != home) {
        store.set(
            HOME_VALUE,
            &WinEnvValue {
                data: home,
                expand: false,
            },
        )?;
        change.modified.push(store.describe(HOME_VALUE));
    }

    if change.changed() {
        change.broadcast = store.notify();
    }
    Ok(change)
}

/// Remove `ANVIL_HOME\bin` from the user `Path` and `ANVIL_HOME` (if it is ours).
pub fn remove_windows_env(store: &mut dyn WinEnvStore, layout: &InstallLayout) -> Result<EnvChange> {
    let bin = layout.bin_dir().display().to_string();
    let home = layout.root().display().to_string();
    let mut change = EnvChange::default();

    change.targets.push(store.describe(PATH_VALUE));
    if let Some(current) = store.get(PATH_VALUE)? {
        let edit = winpath::remove_entry(&current.data, &bin);
        if edit.changed {
            if edit.value.is_empty() {
                store.delete(PATH_VALUE)?;
            } else {
                store.set(
                    PATH_VALUE,
                    &WinEnvValue {
                        data: edit.value,
                        expand: current.expand,
                    },
                )?;
            }
            change.modified.push(store.describe(PATH_VALUE));
        }
    }

    change.targets.push(store.describe(HOME_VALUE));
    if store
        .get(HOME_VALUE)?
        .is_some_and(|v| winpath::entries_equal(&v.data, &home))
    {
        store.delete(HOME_VALUE)?;
        change.modified.push(store.describe(HOME_VALUE));
    }

    if change.changed() {
        change.broadcast = store.notify();
    }
    Ok(change)
}

pub fn windows_env_status(store: &dyn WinEnvStore, layout: &InstallLayout) -> Result<bool> {
    let bin = layout.bin_dir().display().to_string();
    Ok(store
        .get(PATH_VALUE)?
        .is_some_and(|v| winpath::contains_entry(&v.data, &bin)))
}

/// UTF-16LE + NUL terminator, as stored by `REG_SZ` / `REG_EXPAND_SZ`.
pub fn encode_reg_string(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

/// Decode a `REG_SZ` / `REG_EXPAND_SZ` payload (tolerates missing or extra NULs).
pub fn decode_reg_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

mod registry {
    use super::{decode_reg_string, encode_reg_string, WinEnvStore, WinEnvValue};
    use crate::error::{EnvError, Result};
    use winreg::enums::{RegType, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::{RegKey, RegValue};

    const SUBKEY: &str = "Environment";

    /// The real `HKCU\Environment` key.
    pub struct RegistryEnvStore {
        key: RegKey,
    }

    fn reg_err(context: String) -> impl FnOnce(std::io::Error) -> EnvError {
        move |source| EnvError::Registry { context, source }
    }

    impl RegistryEnvStore {
        pub fn open() -> Result<Self> {
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let (key, _) = hkcu
                .create_subkey_with_flags(SUBKEY, KEY_READ | KEY_WRITE)
                .map_err(reg_err(r"opening HKCU\Environment".into()))?;
            Ok(Self { key })
        }
    }

    impl WinEnvStore for RegistryEnvStore {
        fn describe(&self, name: &str) -> String {
            format!(r"HKCU\Environment\{name}")
        }

        fn get(&self, name: &str) -> Result<Option<WinEnvValue>> {
            match self.key.get_raw_value(name) {
                Ok(raw) => match raw.vtype {
                    RegType::REG_SZ | RegType::REG_EXPAND_SZ => Ok(Some(WinEnvValue {
                        data: decode_reg_string(&raw.bytes),
                        expand: raw.vtype == RegType::REG_EXPAND_SZ,
                    })),
                    other => Err(EnvError::Registry {
                        context: format!(r"HKCU\Environment\{name} has unexpected type {other:?}"),
                        source: std::io::Error::from(std::io::ErrorKind::InvalidData),
                    }),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(reg_err(format!(r"reading HKCU\Environment\{name}"))(e)),
            }
        }

        fn set(&mut self, name: &str, value: &WinEnvValue) -> Result<()> {
            let raw = RegValue {
                bytes: encode_reg_string(&value.data),
                vtype: if value.expand {
                    RegType::REG_EXPAND_SZ
                } else {
                    RegType::REG_SZ
                },
            };
            self.key
                .set_raw_value(name, &raw)
                .map_err(reg_err(format!(r"writing HKCU\Environment\{name}")))
        }

        fn delete(&mut self, name: &str) -> Result<()> {
            match self.key.delete_value(name) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(reg_err(format!(r"deleting HKCU\Environment\{name}"))(e)),
            }
        }

        fn notify(&self) -> bool {
            broadcast_environment_change()
        }
    }

    /// `WM_SETTINGCHANGE` with `lParam = "Environment"` to all top-level windows.
    pub fn broadcast_environment_change() -> bool {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
        };
        let param: Vec<u16> = "Environment".encode_utf16().chain([0]).collect();
        let mut result: usize = 0;
        // SAFETY: `param` is a valid NUL-terminated UTF-16 string that outlives the
        // call; `result` is a valid out pointer.
        let ok = unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                param.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                2000,
                &mut result,
            )
        };
        ok != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYOUT_ROOT: &str = r"C:\Users\Eve\AppData\Local\anvil";

    fn store() -> (tempfile::TempDir, FileEnvStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = FileEnvStore::new(dir.path().join(FileEnvStore::FILE_NAME));
        (dir, store)
    }

    fn layout() -> InstallLayout {
        InstallLayout::new(PathBuf::from(LAYOUT_ROOT))
    }

    #[test]
    fn reg_string_round_trip() {
        let s = r"C:\Ünïcødé\bin;%USERPROFILE%\x";
        let bytes = encode_reg_string(s);
        assert_eq!(&bytes[bytes.len() - 2..], &[0, 0]);
        assert_eq!(decode_reg_string(&bytes), s);
        assert_eq!(decode_reg_string(&bytes[..bytes.len() - 2]), s, "missing NUL tolerated");
        assert_eq!(decode_reg_string(&[]), "");
    }

    #[test]
    fn windows_apply_preserves_type_and_is_idempotent() {
        let (_dir, mut store) = store();
        let layout = layout();
        let bin = layout.bin_dir().display().to_string();
        store
            .set(
                "PATH", // different case than "Path": must be matched case-insensitively
                &WinEnvValue {
                    data: r"%USERPROFILE%\.cargo\bin;C:\Tools".into(),
                    expand: true,
                },
            )
            .unwrap();

        let first = apply_windows_env(&mut store, &layout).unwrap();
        assert_eq!(first.modified.len(), 2);
        let path = store.get("Path").unwrap().unwrap();
        assert!(path.expand, "REG_EXPAND_SZ must be preserved");
        assert_eq!(path.data, format!(r"{bin};%USERPROFILE%\.cargo\bin;C:\Tools"));
        assert_eq!(
            store.get("ANVIL_HOME").unwrap().unwrap().data,
            layout.root().display().to_string()
        );
        assert!(windows_env_status(&store, &layout).unwrap());

        let second = apply_windows_env(&mut store, &layout).unwrap();
        assert!(!second.changed(), "{second:?}");
        assert_eq!(store.get("Path").unwrap().unwrap(), path);

        let removed = remove_windows_env(&mut store, &layout).unwrap();
        assert!(removed.changed());
        let path = store.get("Path").unwrap().unwrap();
        assert_eq!(path.data, r"%USERPROFILE%\.cargo\bin;C:\Tools");
        assert!(path.expand);
        assert!(store.get("ANVIL_HOME").unwrap().is_none());
        assert!(!windows_env_status(&store, &layout).unwrap());
        assert!(!remove_windows_env(&mut store, &layout).unwrap().changed());
    }

    #[test]
    fn windows_apply_keeps_reg_sz_and_creates_missing_path() {
        let (_dir, mut store) = store();
        let layout = layout();
        store
            .set(
                "Path",
                &WinEnvValue {
                    data: r"C:\Tools".into(),
                    expand: false,
                },
            )
            .unwrap();
        apply_windows_env(&mut store, &layout).unwrap();
        assert!(!store.get("Path").unwrap().unwrap().expand, "REG_SZ must be preserved");

        let (_dir2, mut empty) = self::store();
        apply_windows_env(&mut empty, &layout).unwrap();
        let created = empty.get("Path").unwrap().unwrap();
        assert!(created.expand);
        assert_eq!(created.data, layout.bin_dir().display().to_string());
        // Removing our only entry deletes the value we created.
        remove_windows_env(&mut empty, &layout).unwrap();
        assert!(empty.get("Path").unwrap().is_none());
    }

    #[test]
    fn foreign_anvil_home_is_not_deleted() {
        let (_dir, mut store) = store();
        store
            .set(
                "ANVIL_HOME",
                &WinEnvValue {
                    data: r"D:\other".into(),
                    expand: false,
                },
            )
            .unwrap();
        remove_windows_env(&mut store, &layout()).unwrap();
        assert_eq!(store.get("ANVIL_HOME").unwrap().unwrap().data, r"D:\other");
    }

    #[test]
    fn sandbox_mode_from_env_var_value() {
        assert!(EnvMode::Sandbox(PathBuf::from("x")).is_sandbox());
        assert!(!EnvMode::Real.is_sandbox());
    }
}
