//! Comparisons for names that can alias on host filesystems.
/// Distinct names collide if their normalised Unicode case folds match.
pub fn refs_collide(a: &str, b: &str) -> bool {
    a != b && caseless::canonical_caseless_match_str(a, b)
}

#[cfg(test)]
mod tests {
    use super::refs_collide;

    #[test]
    fn detects_case_and_normalisation_aliases() {
        assert!(refs_collide("main", "Main"));
        assert!(refs_collide("Straße", "STRASSE"));
        assert!(refs_collide("café", "cafe\u{301}"));
        assert!(!refs_collide("main", "main"));
        assert!(!refs_collide("main", "other"));
    }
}
