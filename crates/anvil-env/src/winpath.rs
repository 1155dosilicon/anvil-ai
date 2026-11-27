//! Pure manipulation of Windows `PATH` strings (`;`-separated, case-insensitive).
//!
//! Compiled on every platform so the logic is unit-tested everywhere.

/// Split a Windows PATH value, dropping empty segments.
pub fn split_path(value: &str) -> Vec<&str> {
    value.split(';').filter(|s| !s.trim().is_empty()).collect()
}

/// Canonical form used for comparisons: trimmed, unquoted, `/` → `\`,
/// no trailing separator (except for drive roots like `C:\`), lower-case.
pub fn normalize_entry(entry: &str) -> String {
    let mut s = entry.trim().trim_matches('"').trim().replace('/', "\\");
    while s.len() > 1 && s.ends_with('\\') && !(s.len() == 3 && s.as_bytes()[1] == b':') {
        s.pop();
    }
    s.to_lowercase()
}

/// Case-insensitive, separator-insensitive equality of two PATH entries.
pub fn entries_equal(a: &str, b: &str) -> bool {
    normalize_entry(a) == normalize_entry(b)
}

pub fn contains_entry(value: &str, dir: &str) -> bool {
    split_path(value).iter().any(|e| entries_equal(e, dir))
}

/// Remove duplicate entries (keeping the first occurrence and its original spelling).
pub fn dedup_path(value: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    split_path(value)
        .into_iter()
        .filter(|e| seen.insert(normalize_entry(e)))
        .collect::<Vec<_>>()
        .join(";")
}

/// Result of an edit: the new value and whether anything changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathEdit {
    pub value: String,
    pub changed: bool,
}

/// Prepend `dir` unless already present (anywhere, in any spelling).
/// When unchanged the original value is returned byte-for-byte.
pub fn add_entry(value: &str, dir: &str) -> PathEdit {
    if contains_entry(value, dir) {
        return PathEdit {
            value: value.to_string(),
            changed: false,
        };
    }
    let mut entries = vec![dir];
    entries.extend(split_path(value));
    PathEdit {
        value: entries.join(";"),
        changed: true,
    }
}

/// Remove every occurrence of `dir` (in any spelling). Other entries keep their order and spelling.
pub fn remove_entry(value: &str, dir: &str) -> PathEdit {
    if !contains_entry(value, dir) {
        return PathEdit {
            value: value.to_string(),
            changed: false,
        };
    }
    PathEdit {
        value: split_path(value)
            .into_iter()
            .filter(|e| !entries_equal(e, dir))
            .collect::<Vec<_>>()
            .join(";"),
        changed: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIN: &str = r"C:\Users\Eve\AppData\Local\anvil\bin";

    #[test]
    fn split_drops_empty_segments() {
        assert_eq!(split_path(r"C:\a;;C:\b; ;"), vec![r"C:\a", r"C:\b"]);
        assert!(split_path("").is_empty());
    }

    #[test]
    fn normalization_is_case_and_separator_insensitive() {
        assert!(entries_equal(BIN, r"c:\users\eve\appdata\local\ANVIL\BIN\"));
        assert!(entries_equal(BIN, "C:/Users/Eve/AppData/Local/anvil/bin/"));
        assert!(entries_equal(BIN, &format!("\"{BIN}\"")));
        assert!(!entries_equal(BIN, r"C:\Users\Eve\AppData\Local\anvil\bin2"));
        assert_eq!(normalize_entry(r"C:\"), r"c:\");
    }

    #[test]
    fn add_is_idempotent_and_prepends() {
        let original = r"%USERPROFILE%\.cargo\bin;C:\Tools";
        let first = add_entry(original, BIN);
        assert!(first.changed);
        assert_eq!(first.value, format!(r"{BIN};%USERPROFILE%\.cargo\bin;C:\Tools"));

        let second = add_entry(&first.value, BIN);
        assert!(!second.changed);
        assert_eq!(second.value, first.value);

        // Different spelling of the same directory is recognised.
        let third = add_entry(&first.value, &BIN.to_uppercase());
        assert!(!third.changed);
        assert_eq!(add_entry("", BIN).value, BIN);
    }

    #[test]
    fn remove_preserves_other_entries() {
        let value = format!(r"C:\A;{};C:\B;{}\;C:\a", BIN.to_lowercase(), BIN);
        let edit = remove_entry(&value, BIN);
        assert!(edit.changed);
        assert_eq!(edit.value, r"C:\A;C:\B;C:\a");
        let again = remove_entry(&edit.value, BIN);
        assert!(!again.changed);
        assert_eq!(again.value, edit.value);
    }

    #[test]
    fn dedup_keeps_first_spelling() {
        assert_eq!(dedup_path(r"C:\A;c:\a\;C:\B;;C:/b"), r"C:\A;C:\B");
    }

    #[test]
    fn add_then_remove_round_trips() {
        let original = r"C:\Windows;C:\Tools";
        let added = add_entry(original, BIN);
        assert_eq!(remove_entry(&added.value, BIN).value, original);
    }
}
