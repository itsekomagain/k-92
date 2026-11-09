//! What a backup leaves out: chosen folders, name patterns (`*.iso`, `node_modules`), files over a
//! size limit, and an optional built-in list of temporary/system junk.
//!
//! Matching is case-insensitive, as on Windows and default macOS (APFS). Excluded items are not recorded as skipped (they
//! were left out on purpose); the snapshot only counts them.

use std::path::{Path, PathBuf};

use k92_core::{Error, Result};

/// Built-in "temporary and system files" list (used when `common_junk` is on).
pub const COMMON_JUNK: &[&str] = &[
    "$RECYCLE.BIN",
    "System Volume Information",
    "pagefile.sys",
    "hiberfil.sys",
    "swapfile.sys",
    "Thumbs.db",
    "~$*",
    "*.tmp",
    ".DS_Store",
    ".Spotlight-V100",
    ".fseventsd",
    ".Trashes",
    ".TemporaryItems",
    ".DocumentRevisions-V100",
    "node_modules",
    "__pycache__",
];

const MAX_PATTERN_LEN: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// An absolute folder (or file) path; everything inside it is excluded.
    Path(String),
    /// A name pattern with `*` and `?`, matched against each file and folder name.
    Name(String),
}

impl Rule {
    /// Parses one user-entered rule. Anything containing a path separator must be an absolute
    /// path; anything else is a name pattern.
    pub fn parse(raw: &str) -> Result<Rule> {
        let s = raw.trim();
        let bad = |why: &str| {
            Err(Error::InvalidRequest(format!(
                "cannot exclude \"{s}\": {why}"
            )))
        };
        if s.is_empty() {
            return bad("empty");
        }
        if s.len() > MAX_PATTERN_LEN || s.chars().any(char::is_control) {
            return bad("not a valid name or path");
        }
        if s.contains(['/', '\\']) {
            if !Path::new(s).is_absolute() {
                return bad("use a full folder path, such as D:\\Work\\Old, or just a name");
            }
            Ok(Rule::Path(norm_path(Path::new(s))))
        } else {
            if s.chars().all(|c| c == '*' || c == '?') {
                return bad("this would exclude everything");
            }
            Ok(Rule::Name(s.to_lowercase()))
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExcludeRules {
    paths: Vec<String>,
    names: Vec<Vec<char>>,
    max_file_bytes: Option<u64>,
}

impl ExcludeRules {
    pub fn new(rules: &[String], common_junk: bool, max_file_bytes: Option<u64>) -> Result<Self> {
        let mut out = ExcludeRules {
            max_file_bytes,
            ..Default::default()
        };
        let junk = COMMON_JUNK
            .iter()
            .filter(|_| common_junk)
            .map(|s| s.to_string());
        for raw in rules.iter().cloned().chain(junk) {
            match Rule::parse(&raw)? {
                Rule::Path(p) => out.paths.push(p),
                Rule::Name(n) => out.names.push(n.chars().collect()),
            }
        }
        Ok(out)
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.names.is_empty() && self.max_file_bytes.is_none()
    }

    fn by_name_or_path(&self, abs: &Path, name: &str) -> bool {
        let lname: Vec<char> = name.to_lowercase().chars().collect();
        if self.names.iter().any(|p| glob(p, &lname)) {
            return true;
        }
        if self.paths.is_empty() {
            return false;
        }
        let p = norm_path(abs);
        self.paths
            .iter()
            .any(|x| p == *x || (p.starts_with(x.as_str()) && p[x.len()..].starts_with('/')))
    }

    pub fn excludes_dir(&self, abs: &Path, name: &str) -> bool {
        self.by_name_or_path(abs, name)
    }

    pub fn excludes_file(&self, abs: &Path, name: &str, size: u64) -> bool {
        self.max_file_bytes.is_some_and(|m| size > m) || self.by_name_or_path(abs, name)
    }
}

/// Lowercase, `/`-separated, without a trailing separator (except a bare root).
fn norm_path(p: &Path) -> String {
    let abs: PathBuf = std::path::absolute(p).unwrap_or_else(|_| p.to_owned());
    let mut s = abs.display().to_string().replace('\\', "/").to_lowercase();
    while s.len() > 1 && s.ends_with('/') && !s.ends_with(":/") {
        s.pop();
    }
    s
}

/// Wildcard match: `*` = any run of characters, `?` = exactly one.
fn glob(pat: &[char], s: &[char]) -> bool {
    let (mut p, mut i) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while i < s.len() {
        if p < pat.len() && (pat[p] == '?' || pat[p] == s[i]) {
            p += 1;
            i += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            mark = i;
            p += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(p: &str, s: &str) -> bool {
        glob(
            &p.chars().collect::<Vec<_>>(),
            &s.chars().collect::<Vec<_>>(),
        )
    }

    #[test]
    fn glob_matches_wildcards() {
        assert!(g("*.iso", "ubuntu.iso"));
        assert!(!g("*.iso", "ubuntu.iso.txt"));
        assert!(g("~$*", "~$budget.xlsx"));
        assert!(g("a?c", "abc"));
        assert!(!g("a?c", "ac"));
        assert!(g("*", ""));
        assert!(g("node_modules", "node_modules"));
        assert!(!g("node_modules", "node_modules2"));
        assert!(g("*a*b*", "xxaxxbxx"));
    }

    #[test]
    fn parse_rejects_bad_rules() {
        assert!(Rule::parse("").is_err());
        assert!(Rule::parse("*").is_err());
        assert!(Rule::parse("old/stuff").is_err(), "relative path");
        assert!(Rule::parse("a\u{0}b").is_err());
        assert_eq!(Rule::parse(" *.ISO ").unwrap(), Rule::Name("*.iso".into()));
    }

    #[test]
    fn rules_match_names_paths_and_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("Docs").join("Old");
        let r = ExcludeRules::new(
            &["*.iso".into(), old.display().to_string()],
            true,
            Some(100),
        )
        .unwrap();
        assert!(r.excludes_file(&dir.path().join("x/Ubuntu.ISO"), "Ubuntu.ISO", 1));
        assert!(r.excludes_dir(&old, "Old"));
        assert!(r.excludes_file(&old.join("a.txt"), "a.txt", 1));
        assert!(!r.excludes_dir(&dir.path().join("Docs").join("Older"), "Older"));
        assert!(r.excludes_dir(&dir.path().join("node_modules"), "node_modules"));
        assert!(r.excludes_file(&dir.path().join("~$a.docx"), "~$a.docx", 1));
        assert!(r.excludes_file(&dir.path().join("big.bin"), "big.bin", 101));
        assert!(!r.excludes_file(&dir.path().join("ok.bin"), "ok.bin", 100));

        let none = ExcludeRules::new(&[], false, None).unwrap();
        assert!(none.is_empty());
        assert!(!none.excludes_dir(&dir.path().join("node_modules"), "node_modules"));
    }
}
