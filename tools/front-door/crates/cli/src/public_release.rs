//! Publication policy. Git/history orchestration awaits a byte-preserving,
//! stdin-capable bounded evidence runner; no command ownership switches here.
use provenant_supervisor::fs::SafeFs;
use regex::bytes::{Regex, RegexBuilder};
use std::{
    collections::BTreeSet,
    path::Path,
};
use thiserror::Error;

pub const MAX_PUBLIC_FILE_BYTES: u64 = 5 * 1024 * 1024;
pub const REQUIRED: &[&str] = &[
    "LICENSE",
    "NOTICE",
    "THIRD_PARTY_NOTICES.md",
    "LICENSES/grill-me-pocock-MIT.txt",
    "LICENSES/impeccable-APACHE-2.0.txt",
    "LICENSES/modern-screenshot-MIT.txt",
    "LICENSES/skill-optimizer-MIT.txt",
    "LICENSES/ui-ux-pro-max-MIT.txt",
    "ACKNOWLEDGEMENTS.md",
    "README.md",
    "MAINTAINING.md",
    "SECURITY.md",
    "docs/ARCHITECTURE.md",
    "docs/worktrees.md",
];
const FORBIDDEN: &[&str] = &[
    ".DS_Store",
    ".claude/settings.local.json",
    "plugins/marketplace.json",
];
const FORBIDDEN_PREFIXES: &[&str] = &[
    ".agent-run/",
    ".worktrees/",
    ".pytest_cache/",
    "skills/clean-writing/",
    "skills/academic-writing/",
    "skills/humanise-text/",
    "skills/tanstack-query-best-practices/",
    "skills/vercel-react-best-practices/",
    "skills/playwright/",
    "skills/react-performance/",
    "skills/tanstack-query/",
    "skills/typescript-clean-code/",
    "skills/uml-diagrams/",
    "skills/web-stack-conventions/",
];
// Source order is observable when one file matches multiple classes.
const PATTERNS: &[(&str, &str)] = &[
    (
        "personal absolute home path",
        r"/(?:Users|home)/[A-Za-z0-9._-]+/",
    ),
    (
        "possible private key",
        r"-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----",
    ),
    (
        "possible GitHub token",
        r"\b(?:gh[opusr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,})\b",
    ),
    (
        "possible OpenAI key",
        r"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b",
    ),
    ("possible Anthropic key", r"\bsk-ant-[A-Za-z0-9_-]{20,}\b"),
    ("possible AWS access key", r"\bAKIA[0-9A-Z]{16}\b"),
];

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Pattern(#[from] regex::Error),
    #[error("publication filesystem inspection failed for {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub struct Scanner {
    patterns: Vec<(&'static str, Regex)>,
}
impl Scanner {
    pub fn new() -> Result<Self, Error> {
        let patterns = PATTERNS
            .iter()
            .map(|&(label, pattern)| {
                Ok((label, RegexBuilder::new(pattern).unicode(false).build()?))
            })
            .collect::<Result<_, regex::Error>>()?;
        Ok(Self { patterns })
    }
    pub fn classify(&self, raw: &[u8]) -> BTreeSet<String> {
        self.patterns
            .iter()
            .filter(|(_, pattern)| pattern.is_match(raw))
            .map(|(label, _)| (*label).to_owned())
            .collect()
    }
    pub fn grep_arguments(&self) -> Vec<String> {
        PATTERNS
            .iter()
            .flat_map(|(_, pattern)| ["-e".into(), pattern.replace("(?:", "(").replace(r"\b", "")])
            .collect()
    }
    /// Current-tree policy, with held safe file handles for content reads.
    /// Metadata and unreadable/invalid text handling follow scan_paths exactly.
    pub fn scan_paths(
        &self,
        root: &Path,
        paths: &[String],
        fs: &impl SafeFs,
    ) -> Result<Vec<String>, Error> {
        let mut errors = Vec::new();
        for required in required_sorted() {
            if !root.join(required).is_file() {
                errors.push(format!("missing required public file: {required}"));
            } else if !paths.iter().any(|path| path == required) {
                errors.push(format!("required public file is not tracked: {required}"));
            }
        }
        for relative in paths {
            if is_forbidden(relative) {
                errors.push(format!("forbidden tracked path: {relative}"));
                continue;
            }
            let path = root.join(relative);
            if path.is_symlink() {
                errors.push(format!("tracked symlink is not portable: {relative}"));
                continue;
            }
            if !path.is_file() {
                continue;
            }
            let metadata = std::fs::metadata(&path).map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
            if metadata.len() > MAX_PUBLIC_FILE_BYTES {
                errors.push(format!("tracked file exceeds 5 MiB: {relative}"));
                continue;
            }
            // Python ignores a read failure and invalid UTF-8 after the stat.
            let read = (|| {
                use std::io::Read;
                let file = fs.open_source(&path)?;
                let mut text = String::new();
                file.take(MAX_PUBLIC_FILE_BYTES + 1)
                    .read_to_string(&mut text)?;
                Ok::<_, std::io::Error>(text)
            })();
            let Ok(text) = read else {
                continue;
            };
            // Regex over str in Python has Unicode word boundaries; use the
            // same ASCII token patterns but test boundaries with core semantics.
            for (label, pattern) in &self.patterns {
                let found = pattern.find_iter(text.as_bytes()).any(|m| {
                    if !PATTERNS
                        .iter()
                        .find(|(l, _)| l == label)
                        .is_some_and(|(_, p)| p.starts_with(r"\b"))
                    {
                        return true;
                    }
                    text[..m.start()]
                        .chars()
                        .next_back()
                        .is_none_or(|c| !crate::pending::python_word_character(c))
                        && text[m.end()..]
                            .chars()
                            .next()
                            .is_none_or(|c| !crate::pending::python_word_character(c))
                });
                if found {
                    errors.push(format!("{label}: {relative}"));
                }
            }
        }
        Ok(errors)
    }
}

fn required_sorted() -> Vec<&'static str> {
    let mut values = REQUIRED.to_vec();
    values.sort();
    values
}
pub fn is_forbidden(path: &str) -> bool {
    FORBIDDEN.contains(&path)
        || FORBIDDEN_PREFIXES
            .iter()
            .any(|prefix| path.starts_with(prefix))
}
pub fn forbidden_paths(paths: impl IntoIterator<Item = String>) -> Vec<String> {
    paths
        .into_iter()
        .filter(|path| is_forbidden(path))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Tuple wire shape from ls-tree: mode, kind, object ID, optional size, path.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TreeEntry(
    pub String,
    pub String,
    pub String,
    pub Option<Size>,
    pub String,
);
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Size {
    Signed(i64),
    Unsigned(u64),
}
impl Size {
    fn exceeds(&self, limit: u64) -> bool {
        match self {
            Self::Signed(n) => i128::from(*n) > i128::from(limit),
            Self::Unsigned(n) => *n > limit,
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    #[test]
    fn byte_registry_and_prefilter_match_source_including_invalid_utf8() {
        let scanner = Scanner::new().unwrap();
        let mut cases = vec![
            b"ordinary text".to_vec(),
            [b"\xff/Users/".as_slice(), b"alice/private"].concat(),
        ];
        for text in [
            "-----BEGIN OPENSSH PRIVATE KEY-----",
            "ghp_",
            "github_pat_",
            "sk-proj-",
            "sk-ant-",
            "AKIA",
        ] {
            for prefix in ["", "a", "é", "_", "\n"] {
                for suffix in ["", "a", "é", "_", "\n"] {
                    cases.push(
                        format!(
                            "{prefix}{text}{}{suffix}",
                            "A".repeat(if text == "AKIA" { 16 } else { 20 })
                        )
                        .into_bytes(),
                    );
                }
            }
        }
        let Some(expected): Option<(Vec<BTreeSet<String>>,Vec<String>)> = crate::oracle::python(
            "import json,sys; from scripts.public_release_check import classify,GREP_ARGUMENTS; x=json.load(sys.stdin); print(json.dumps([[sorted(classify(bytes(v))) for v in x],GREP_ARGUMENTS]))", &json!(cases)
        ) else { return; };
        assert_eq!(scanner.grep_arguments(), expected.1);
        for (case, expected) in cases.iter().zip(expected.0) {
            assert_eq!(scanner.classify(case), expected, "{case:?}");
        }
    }
    #[test]
    fn current_tree_safe_file_reads_match_source() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "safe").unwrap();
        let paths = [
            "README.md",
            "tokens",
            "unicode-boundary",
            "binary",
            "link",
            "large",
            ".agent-run/x",
            "absent",
        ];
        std::fs::write(
            dir.path().join("tokens"),
            format!("/home/{}\nsk-ant-{}", "alice/private", "x".repeat(25)),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("unicode-boundary"),
            format!("éghp_{}é", "x".repeat(25)),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("binary"),
            [b"\xff/home/".as_slice(), b"alice/private"].concat(),
        )
        .unwrap();
        std::os::unix::fs::symlink("README.md", dir.path().join("link")).unwrap();
        std::fs::File::create(dir.path().join("large"))
            .unwrap()
            .set_len(MAX_PUBLIC_FILE_BYTES + 1)
            .unwrap();
        let Some(expected): Option<Vec<String>> = crate::oracle::python(
            "import json,sys; from pathlib import Path; from scripts.public_release_check import scan_paths; x=json.load(sys.stdin); print(json.dumps(scan_paths(x['paths'],Path(x['root']))))", &json!({"paths":paths,"root":dir.path()})
        ) else { return; };
        assert_eq!(
            Scanner::new()
                .unwrap()
                .scan_paths(
                    dir.path(),
                    &paths.map(str::to_owned),
                    &provenant_supervisor::fs::LocalFs
                )
                .unwrap(),
            expected
        );
    }
}
