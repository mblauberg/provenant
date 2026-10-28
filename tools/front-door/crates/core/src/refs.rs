//! Comparisons for names that can alias on host filesystems.
/// Distinct names collide if their normalised Unicode case folds match.
pub fn refs_collide(a: &str, b: &str) -> bool {
    if a == b {
        return false;
    }
    let mut left = a.split('/');
    let mut right = b.split('/');
    loop {
        match (left.next(), right.next()) {
            (Some(x), Some(y)) if caseless::canonical_caseless_match_str(x, y) => {}
            (None, _) | (_, None) => return true,
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::refs_collide;

    #[test]
    fn detects_case_and_normalisation_aliases() {
        assert!(refs_collide("main", "Main"));
        assert!(refs_collide("Straße", "STRASSE"));
        assert!(refs_collide("café", "cafe\u{301}"));
        assert!(refs_collide("Feature", "feature/x"));
        assert!(refs_collide("feature/x", "Feature"));
        assert!(!refs_collide("feature/x", "feature/y"));
        assert!(!refs_collide("main", "main"));
        assert!(!refs_collide("main", "other"));
    }
}
