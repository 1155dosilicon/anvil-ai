use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// Host platforms supported by Anvil bundles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Platform {
    #[serde(rename = "windows-x86_64")]
    WindowsX86_64,
    #[serde(rename = "linux-x86_64")]
    LinuxX86_64,
}

impl Platform {
    pub const ALL: [Platform; 2] = [Platform::WindowsX86_64, Platform::LinuxX86_64];

    /// Platform of the running process, or `None` if it is not supported.
    pub fn current() -> Option<Platform> {
        if cfg!(all(windows, target_arch = "x86_64")) {
            Some(Platform::WindowsX86_64)
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Some(Platform::LinuxX86_64)
        } else {
            None
        }
    }

    /// Like [`Platform::current`] but returns an error for unsupported hosts.
    pub fn require_current() -> Result<Platform, CoreError> {
        Self::current().ok_or_else(|| {
            CoreError::UnsupportedPlatform(format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH))
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Platform::WindowsX86_64 => "windows-x86_64",
            Platform::LinuxX86_64 => "linux-x86_64",
        }
    }

    /// Executable suffix used by binaries of this platform.
    pub fn exe_suffix(self) -> &'static str {
        match self {
            Platform::WindowsX86_64 => ".exe",
            Platform::LinuxX86_64 => "",
        }
    }

    pub fn is_windows(self) -> bool {
        matches!(self, Platform::WindowsX86_64)
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Platform {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Platform::ALL
            .into_iter()
            .find(|p| p.as_str() == s)
            .ok_or_else(|| CoreError::UnsupportedPlatform(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_strings_and_json() {
        for p in Platform::ALL {
            assert_eq!(p.as_str().parse::<Platform>().unwrap(), p);
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(json, format!("\"{}\"", p.as_str()));
            assert_eq!(serde_json::from_str::<Platform>(&json).unwrap(), p);
        }
        assert!("macos-aarch64".parse::<Platform>().is_err());
    }

    #[test]
    fn current_platform_is_detected_on_supported_hosts() {
        if cfg!(all(windows, target_arch = "x86_64")) {
            assert_eq!(Platform::current(), Some(Platform::WindowsX86_64));
            assert_eq!(Platform::WindowsX86_64.exe_suffix(), std::env::consts::EXE_SUFFIX);
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(Platform::current(), Some(Platform::LinuxX86_64));
            assert_eq!(Platform::LinuxX86_64.exe_suffix(), std::env::consts::EXE_SUFFIX);
        }
    }
}
