//! Repo paths a writer may change without forcing review.
//!
//! Every implementation touches some paths (release-note fragments, specs,
//! lock files). The host lists them per project; a committed change to a listed
//! path never makes a lane `review_required`, and a listed path left uncommitted
//! never makes it `uncommitted`. Paths that change what the host or an agent
//! executes (Git metadata, hooks, provider control roots) can never be listed.
use crate::quarantine::is_hard_control;
use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};

/// Longest accepted pattern, in bytes.
pub const MAX_PATTERN_BYTES: usize = 256;
/// Most patterns in one list.
pub const MAX_PATTERNS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Shape {
    /// `docs/specs/`: everything below a directory at the repository root.
    Directory,
    /// `Cargo.lock`: a file of that name at any depth.
    Name,
    /// `docs/changelog.md`: exactly that path from the repository root.
    Path,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pattern {
    text: String,
    shape: Shape,
}

/// A validated list of exempt patterns. The empty list exempts nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewExempt {
    patterns: Vec<Pattern>,
}

/// Whether `text` is an acceptable pattern: relative, `/`-separated, without
/// wildcards, `.` or `..` components, control characters or backslashes.
pub fn valid_pattern(text: &str) -> bool {
    let body = text.strip_suffix('/').unwrap_or(text);
    !body.is_empty()
        && text.len() <= MAX_PATTERN_BYTES
        && !text.starts_with('/')
        && !text.chars().any(|c| c.is_control() || "\\*?[]".contains(c))
        && body
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

impl ReviewExempt {
    pub fn none() -> Self {
        Self::default()
    }

    /// Compile validated patterns; an invalid one is skipped, since a
    /// narrower list only adds review.
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Self {
        let patterns = patterns
            .iter()
            .map(AsRef::as_ref)
            .filter(|text| valid_pattern(text))
            .take(MAX_PATTERNS)
            .map(|text| Pattern {
                text: text.to_owned(),
                shape: if text.ends_with('/') {
                    Shape::Directory
                } else if text.contains('/') {
                    Shape::Path
                } else {
                    Shape::Name
                },
            })
            .collect();
        Self { patterns }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// The patterns as configured, for display.
    pub fn patterns(&self) -> Vec<&str> {
        self.patterns.iter().map(|p| p.text.as_str()).collect()
    }

    /// Whether `path` (repository-relative bytes) is listed and may be listed.
    /// Names compare without ASCII case, as the platform's file systems do.
    pub fn covers(&self, path: &[u8]) -> bool {
        if self.patterns.is_empty() || is_hard_control(Path::new(OsStr::from_bytes(path))) {
            return false;
        }
        self.patterns.iter().any(|pattern| {
            let text = pattern.text.as_bytes();
            match pattern.shape {
                Shape::Directory => {
                    path.len() > text.len() && path[..text.len()].eq_ignore_ascii_case(text)
                }
                Shape::Path => path.eq_ignore_ascii_case(text),
                Shape::Name => path
                    .rsplit(|byte| *byte == b'/')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(text)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> ReviewExempt {
        ReviewExempt::new(&[
            "docs/release-notes/next.d/",
            "docs/specs/",
            "Cargo.lock",
            ".tracker-input",
        ])
    }

    #[test]
    fn directories_names_and_exact_paths_match_their_shapes() {
        let exempt = defaults();
        for covered in [
            "docs/release-notes/next.d/2181-x.md",
            "docs/specs/run.md",
            "docs/specs/deep/er.md",
            "Cargo.lock",
            "tools/front-door/Cargo.lock",
            ".tracker-input",
            "DOCS/Specs/run.md",
        ] {
            assert!(exempt.covers(covered.as_bytes()), "{covered}");
        }
        for uncovered in [
            "docs/specs",
            "docs/spec/run.md",
            "src/docs/specs/run.md",
            "Cargo.lock.bak",
            "tools/Cargo.lock/x",
            "src/lib.rs",
        ] {
            assert!(!exempt.covers(uncovered.as_bytes()), "{uncovered}");
        }
        let exact = ReviewExempt::new(&["docs/changelog.md"]);
        assert!(exact.covers(b"docs/changelog.md"));
        assert!(!exact.covers(b"x/docs/changelog.md"));
        assert!(!ReviewExempt::none().covers(b"Cargo.lock"));
    }

    #[test]
    fn hard_controls_can_never_be_exempted() {
        let exempt = ReviewExempt::new(&[
            ".git/",
            ".githooks/",
            "hooks/",
            ".claude/",
            ".mcp.json",
            ".gitattributes",
            "settings.json",
            ".github/",
            "AGENTS.md",
            ".agents/",
        ]);
        for hard in [
            ".git/config",
            ".githooks/pre-commit",
            "hooks/pre-push",
            ".claude/settings.json",
            ".mcp.json",
            "sub/.gitattributes",
            ".claude/settings.json",
            ".github/workflows/ci.yml",
            "x/.gitmodules",
        ] {
            assert!(!exempt.covers(hard.as_bytes()), "{hard}");
        }
        // An instruction document or the instruction tree can be listed.
        assert!(exempt.covers(b"docs/AGENTS.md"));
        assert!(exempt.covers(b".agents/notes/a.md"));
    }

    #[test]
    fn patterns_are_relative_plain_and_bounded() {
        for good in ["Cargo.lock", "docs/", "a/b.md", ".tracker-input"] {
            assert!(valid_pattern(good), "{good}");
        }
        for bad in [
            "",
            "/",
            "/abs",
            "../x",
            "a/../b",
            "a//b",
            "./x",
            "*.lock",
            "a?b",
            "a\\b",
            "a\nb",
            &"x".repeat(MAX_PATTERN_BYTES + 1),
        ] {
            assert!(!valid_pattern(bad), "{bad:?}");
        }
        assert!(ReviewExempt::new(&["../x", "*"]).is_empty());
        let many: Vec<String> = (0..MAX_PATTERNS + 3).map(|n| format!("d{n}/")).collect();
        assert_eq!(ReviewExempt::new(&many).patterns().len(), MAX_PATTERNS);
    }
}
