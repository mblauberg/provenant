//! Comparisons for names that can alias on host filesystems, and revision parsing.

/// Distinct names collide if their lowercase forms match, component by
/// component, or one is a path prefix of the other.
pub fn refs_collide(a: &str, b: &str) -> bool {
    if a == b {
        return false;
    }
    let mut left = a.split('/');
    let mut right = b.split('/');
    loop {
        match (left.next(), right.next()) {
            (Some(x), Some(y)) if x.to_lowercase() == y.to_lowercase() => {}
            (None, _) | (_, None) => return true,
            _ => return false,
        }
    }
}

/// Split a revision expression into the ref-like root and the operators
/// applied to it (`origin/main~1` into `origin/main` and `~1`). The upstream
/// forms `@{u}`, `@{upstream}` and `@{push}` belong to the root.
pub fn split_revision_root(revision: &str) -> (&str, &str) {
    let mut end = revision.find(['^', '~', ':']).unwrap_or(revision.len());
    if let Some(at) = revision[..end].find("@{") {
        let upstream = revision[at..end]
            .split_inclusive('}')
            .next()
            .filter(|form| matches!(*form, "@{u}" | "@{upstream}" | "@{push}"));
        end = at + upstream.map_or(0, str::len);
    }
    if end == 0 {
        (revision, "")
    } else {
        revision.split_at(end)
    }
}

#[cfg(test)]
mod tests {
    use super::{refs_collide, split_revision_root};

    #[test]
    fn folds_ascii_and_non_ascii_case() {
        assert!(refs_collide("main", "Main"));
        assert!(refs_collide("Éclair", "éclair"));
        assert!(!refs_collide("main", "other"));
    }

    #[test]
    fn does_not_normalise_unicode() {
        assert!(!refs_collide("café", "cafe\u{301}"));
    }

    #[test]
    fn folds_path_prefixes() {
        assert!(refs_collide("Feature", "feature/x"));
        assert!(refs_collide("feature/x", "Feature"));
        assert!(!refs_collide("feature/x", "feature/y"));
        assert!(!refs_collide("main", "main"));
    }

    #[test]
    fn splits_revision_roots() {
        assert_eq!(split_revision_root("origin/main"), ("origin/main", ""));
        assert_eq!(split_revision_root("origin/main~1"), ("origin/main", "~1"));
        assert_eq!(
            split_revision_root("origin/main^{tree}"),
            ("origin/main", "^{tree}")
        );
        assert_eq!(split_revision_root("@{u}^"), ("@{u}", "^"));
        assert_eq!(
            split_revision_root("origin/main:a.txt"),
            ("origin/main", ":a.txt")
        );
        assert_eq!(
            split_revision_root("origin/main@{1}"),
            ("origin/main", "@{1}")
        );
        assert_eq!(split_revision_root(":/fix"), (":/fix", ""));
    }
}
