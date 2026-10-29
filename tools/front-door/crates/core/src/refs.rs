//! Comparisons for names that can alias on host filesystems, and revision parsing.
use std::collections::HashMap;
use unicode_normalization::UnicodeNormalization;

/// The form under which two names share a path on a host that folds case or
/// Unicode normalisation: NFC, lowercased, NFC again (lowercasing can
/// decompose).
fn folded(component: &str) -> String {
    component
        .nfc()
        .collect::<String>()
        .to_lowercase()
        .nfc()
        .collect()
}

fn same_folded(x: &str, y: &str) -> bool {
    if x.is_ascii() && y.is_ascii() {
        x.eq_ignore_ascii_case(y)
    } else {
        folded(x) == folded(y)
    }
}

fn same_lowercase(x: &str, y: &str) -> bool {
    if x.is_ascii() && y.is_ascii() {
        x.eq_ignore_ascii_case(y)
    } else {
        x.to_lowercase() == y.to_lowercase()
    }
}

fn collide_by(a: &str, b: &str, same: fn(&str, &str) -> bool) -> bool {
    let mut left = a.split('/');
    let mut right = b.split('/');
    loop {
        match (left.next(), right.next()) {
            (Some(x), Some(y)) if same(x, y) => {}
            (None, _) | (_, None) => return true,
            _ => return false,
        }
    }
}

/// Distinct names collide if their NFC-normalised, lowercase forms match,
/// component by component, or one is a path prefix of the other.
pub fn refs_collide(a: &str, b: &str) -> bool {
    a != b && collide_by(a, b, same_folded)
}

/// Distinct names that collide only because a host composes or decomposes
/// Unicode (NFC against NFD spellings), not through case alone. Delivery
/// refuses these outright rather than leaving one out, since which spelling a
/// host keeps is not under its control.
pub fn refs_collide_by_normalisation(a: &str, b: &str) -> bool {
    refs_collide(a, b) && !collide_by(a, b, same_lowercase)
}

/// The first advertised name that collides only by normalisation with another
/// advertised name or with an existing one, with that other name. Indexes the
/// folded forms, so the work stays near-linear in the number of names.
pub fn find_normalisation_alias<'a>(
    advertised: &[&'a str],
    existing: &[&'a str],
) -> Option<(&'a str, &'a str)> {
    let key = |name: &str| name.split('/').map(folded).collect::<Vec<_>>().join("/");
    // Names by folded key, and by each proper path prefix of that key.
    let mut exact: HashMap<String, Vec<&'a str>> = HashMap::new();
    let mut below: HashMap<String, Vec<&'a str>> = HashMap::new();
    for name in advertised.iter().chain(existing) {
        let folded_key = key(name);
        for (at, _) in folded_key.match_indices('/') {
            below
                .entry(folded_key[..at].to_owned())
                .or_default()
                .push(name);
        }
        exact.entry(folded_key).or_default().push(name);
    }
    for name in advertised {
        let folded_key = key(name);
        // Names equal to this one, below it, or at one of its path prefixes.
        let prefixes = folded_key
            .match_indices('/')
            .map(|(at, _)| &folded_key[..at]);
        let candidates = exact
            .get(&folded_key)
            .into_iter()
            .chain(below.get(&folded_key))
            .chain(prefixes.filter_map(|prefix| exact.get(prefix)))
            .flatten();
        for other in candidates {
            if refs_collide_by_normalisation(name, other) {
                return Some((name, other));
            }
        }
    }
    None
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
    use super::{
        find_normalisation_alias, refs_collide, refs_collide_by_normalisation, split_revision_root,
    };

    #[test]
    fn folds_ascii_and_non_ascii_case() {
        assert!(refs_collide("main", "Main"));
        assert!(refs_collide("Éclair", "éclair"));
        assert!(!refs_collide("main", "other"));
    }

    #[test]
    fn folds_unicode_normalisation() {
        let (nfc, nfd) = ("caf\u{e9}", "cafe\u{301}");
        assert!(refs_collide(nfc, nfd));
        assert!(refs_collide("Caf\u{c9}/x", "cafe\u{301}"));
        assert!(refs_collide_by_normalisation(nfc, nfd));
        assert!(refs_collide_by_normalisation("Caf\u{c9}", nfd));
        assert!(refs_collide_by_normalisation(nfd, "caf\u{e9}/x"));
        assert!(!refs_collide_by_normalisation("main", "Main"));
        assert!(!refs_collide_by_normalisation(nfc, nfc));
        assert!(!refs_collide(nfc, "cafe"));
    }

    #[test]
    fn finds_aliases_among_advertised_and_existing_names() {
        let (nfc, nfd) = ("caf\u{e9}", "cafe\u{301}");
        assert_eq!(
            find_normalisation_alias(&["main", nfc, "x", nfd], &[]),
            Some((nfc, nfd))
        );
        assert!(find_normalisation_alias(&["main", nfd], &[nfc]).is_some());
        // The longer name finds the shorter one through its path prefix.
        assert!(find_normalisation_alias(&["Caf\u{c9}/x"], &[nfd]).is_some());
        assert!(find_normalisation_alias(&[nfd], &["caf\u{e9}/x"]).is_some());
        // Names that differ by case alone, equal names, and names that match
        // only among existing refs are not this check's concern.
        assert_eq!(
            find_normalisation_alias(&["main", "Main", nfc], &[nfc]),
            None
        );
        assert_eq!(find_normalisation_alias(&["main"], &[nfc, nfd]), None);
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
