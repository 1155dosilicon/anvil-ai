//! Files placed at the root of the Anvil USB drive (`templates/` in the repository).
//!
//! Embedded so the drive builder (Phase 4) can materialise them from the binary.

/// A file for the root of the USB drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbTemplate {
    pub file_name: &'static str,
    pub contents: &'static str,
    /// Needs the executable bit on Unix.
    pub executable: bool,
}

pub const USB_TEMPLATES: &[UsbTemplate] = &[
    UsbTemplate {
        file_name: "AGENTS.md",
        contents: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../templates/AGENTS.md")),
        executable: false,
    },
    UsbTemplate {
        file_name: "START_HERE.md",
        contents: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../templates/START_HERE.md")),
        executable: false,
    },
    UsbTemplate {
        file_name: "install.ps1",
        contents: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../templates/install.ps1")),
        executable: false,
    },
    UsbTemplate {
        file_name: "install.sh",
        contents: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../templates/install.sh")),
        executable: true,
    },
];

pub fn usb_template(file_name: &str) -> Option<&'static UsbTemplate> {
    USB_TEMPLATES.iter().find(|t| t.file_name == file_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_md_is_short_and_deterministic() {
        let text = usb_template("AGENTS.md").unwrap().contents;
        assert!(text.lines().count() < 60, "AGENTS.md must stay under 60 lines");
        assert!(text.contains("anvil.exe install --yes --json"));
        assert!(text.contains("doctor --smoke-test --json"));
    }

    #[test]
    fn install_scripts_call_anvil_install_yes() {
        let ps1 = usb_template("install.ps1").unwrap();
        assert!(ps1.contents.contains("anvil.exe"));
        assert!(ps1.contents.contains("install --from $PSScriptRoot --yes"));

        let sh = usb_template("install.sh").unwrap();
        assert!(sh.executable);
        assert!(sh.contents.starts_with("#!/bin/sh\n"));
        assert!(!sh.contents.contains('\r'), "install.sh must use LF line endings");
        assert!(sh.contents.contains("install --from \"$here\" --yes"));
    }

    #[test]
    fn start_here_covers_both_platforms() {
        let text = usb_template("START_HERE.md").unwrap().contents;
        assert!(text.contains("## Windows") && text.contains("## Linux"));
    }
}
