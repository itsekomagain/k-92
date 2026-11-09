//! `RelPath`: the only way a path read from a manifest (untrusted) becomes a filesystem path.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A validated, normalized path relative to a backup source root.
///
/// * `/`-separated, no leading or trailing separator; `""` is the root itself.
/// * Every component is non-empty, not `.`/`..`, contains no separators, drive or
///   stream colons, wildcard/reserved Windows characters or control characters, and does not
///   end with a dot or space (Win32 silently strips those, which could alias names).
///
/// Because of these rules, `base.join(rel.to_path())` can never escape `base`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

impl RelPath {
    pub fn root() -> Self {
        RelPath(String::new())
    }

    /// Parses and validates a `/`-separated relative path.
    pub fn parse(s: &str) -> Result<Self> {
        if s.is_empty() {
            return Ok(Self::root());
        }
        for comp in s.split('/') {
            validate_component(comp)?;
        }
        Ok(RelPath(s.to_owned()))
    }

    /// Validates a single path component (a file or directory name).
    pub fn is_valid_component(name: &str) -> bool {
        validate_component(name).is_ok()
    }

    /// Returns `self/name` after validating `name`.
    pub fn join(&self, name: &str) -> Result<Self> {
        validate_component(name)?;
        if self.0.is_empty() {
            Ok(RelPath(name.to_owned()))
        } else {
            Ok(RelPath(format!("{}/{}", self.0, name)))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|c| !c.is_empty())
    }

    pub fn file_name(&self) -> Option<&str> {
        if self.0.is_empty() {
            None
        } else {
            self.0.rsplit('/').next()
        }
    }

    pub fn parent(&self) -> Option<RelPath> {
        if self.0.is_empty() {
            return None;
        }
        Some(match self.0.rfind('/') {
            Some(i) => RelPath(self.0[..i].to_owned()),
            None => RelPath::root(),
        })
    }

    /// True if `self` equals `ancestor` or lies beneath it.
    pub fn starts_with(&self, ancestor: &RelPath) -> bool {
        ancestor.0.is_empty()
            || self.0 == ancestor.0
            || (self.0.starts_with(&ancestor.0) && self.0.as_bytes()[ancestor.0.len()] == b'/')
    }

    /// Strips `ancestor` from the front. Returns `None` if not a descendant.
    pub fn strip_prefix(&self, ancestor: &RelPath) -> Option<RelPath> {
        if !self.starts_with(ancestor) {
            return None;
        }
        if ancestor.0.is_empty() {
            return Some(self.clone());
        }
        let rest = &self.0[ancestor.0.len()..];
        Some(RelPath(rest.trim_start_matches('/').to_owned()))
    }

    /// Converts to a platform relative path, component by component.
    pub fn to_path(&self) -> PathBuf {
        let mut p = PathBuf::new();
        for c in self.components() {
            p.push(c);
        }
        p
    }

    /// `base` joined with this path. Safe because of the component rules.
    pub fn under(&self, base: &Path) -> PathBuf {
        base.join(self.to_path())
    }
}

fn validate_component(c: &str) -> Result<()> {
    let bad = |why: &str| Err(Error::InvalidPath(format!("{c:?}: {why}")));
    if c.is_empty() {
        return bad("empty component");
    }
    if c == "." || c == ".." {
        return bad("relative component");
    }
    if c.ends_with('.') || c.ends_with(' ') {
        return bad("ends with dot or space");
    }
    if c.len() > 1024 {
        return bad("component too long");
    }
    for ch in c.chars() {
        if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            return bad("reserved character");
        }
    }
    Ok(())
}

impl TryFrom<String> for RelPath {
    type Error = Error;
    fn try_from(s: String) -> Result<Self> {
        RelPath::parse(&s)
    }
}

impl From<RelPath> for String {
    fn from(p: RelPath) -> String {
        p.0
    }
}

impl fmt::Debug for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RelPath({:?})", self.0)
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Windows reserved device names (`CON`, `NUL`, `COM1`, `LPT1`, …), matched case-insensitively
/// and with any extension (`nul.txt` is reserved too).
pub fn is_windows_reserved_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && matches!(stem.chars().nth(3), Some('0'..='9' | '¹' | '²' | '³')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_paths() {
        for ok in [
            "",
            "a",
            "a/b.txt",
            "Résumé/naïve 文件.docx",
            ".hidden",
            "a b/c-d_e",
        ] {
            assert!(RelPath::parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_traversal_and_platform_tricks() {
        for bad in [
            "/a",
            "a/",
            "a//b",
            "..",
            "a/../b",
            ".",
            "a/./b",
            "C:",
            "C:/x",
            "a\\b",
            "file.txt:ads",
            "a\0b",
            "trailing.",
            "trailing ",
            "x/y?",
            "\\\\server\\share",
        ] {
            assert!(RelPath::parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn under_never_escapes_base() {
        let base = Path::new("/restore");
        let p = RelPath::parse("a/b/c.txt").unwrap();
        assert!(p.under(base).starts_with(base));
    }

    #[test]
    fn prefix_ops() {
        let p = RelPath::parse("docs/sub/file.txt").unwrap();
        let docs = RelPath::parse("docs").unwrap();
        let doc = RelPath::parse("doc").unwrap();
        assert!(p.starts_with(&docs));
        assert!(!p.starts_with(&doc));
        assert_eq!(p.strip_prefix(&docs).unwrap().as_str(), "sub/file.txt");
        assert_eq!(p.parent().unwrap().as_str(), "docs/sub");
        assert_eq!(docs.parent().unwrap(), RelPath::root());
        assert_eq!(p.file_name(), Some("file.txt"));
    }

    #[test]
    fn deserialization_validates() {
        let r: std::result::Result<RelPath, _> = serde_json::from_str("\"../evil\"");
        assert!(r.is_err());
    }

    #[test]
    fn reserved_names() {
        for r in ["CON", "nul.txt", "Com1", "LPT9.log", "aux"] {
            assert!(is_windows_reserved_name(r), "{r}");
        }
        for ok in ["CONSOLE", "COM10", "nullable.txt", "auxiliary"] {
            assert!(!is_windows_reserved_name(ok), "{ok}");
        }
    }
}
