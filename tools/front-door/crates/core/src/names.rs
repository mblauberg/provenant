//! One spelling for user-facing names: close matches, case folding, aliases
//! and the refusal that carries the candidates. No dependency on records.

/// Compute the Levenshtein distance between strings in Unicode scalar values.
pub fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut row: Vec<_> = (0..=right.len()).collect();
    for (i, a) in left.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, b) in right.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (row[j] + 1)
                .min(above + 1)
                .min(diagonal + usize::from(a != *b));
            diagonal = above;
        }
    }
    row[right.len()]
}

/// Ranked close matches: case-insensitive; exact-after-folding first, then
/// prefix, then substring, then edit distance at most `max(1, len / 3)`; at
/// most five. Input order breaks ties.
pub fn close_matches<'a>(query: &str, universe: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    close_matches_with_cap(query, universe, 5)
}

/// The same ranking with an explicit cap. Callers whose catalogue has a larger
/// historical candidate budget use this instead of reimplementing the ranker.
pub fn close_matches_with_cap<'a>(
    query: &str,
    universe: impl IntoIterator<Item = &'a str>,
    cap: usize,
) -> Vec<&'a str> {
    let folded = query.to_lowercase();
    let limit = (query.chars().count() / 3).max(1);
    let mut ranked = Vec::new();
    for (index, candidate) in universe.into_iter().enumerate() {
        let lowered = candidate.to_lowercase();
        let distance = edit_distance(&folded, &lowered);
        let rank = if lowered == folded {
            0
        } else if lowered.starts_with(&folded) {
            1
        } else if lowered.contains(&folded) {
            2
        } else if distance <= limit {
            3
        } else {
            continue;
        };
        ranked.push((rank, distance, index, candidate));
    }
    ranked.sort_by_key(|(rank, distance, index, _)| (*rank, *distance, *index));
    ranked
        .into_iter()
        .take(cap)
        .map(|(_, _, _, candidate)| candidate)
        .collect()
}

/// What kind of name a refusal or fold is about. `"model"`, `"adapter"`,
/// `"account"`, `"graph"`, `"command"` and so on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NameKind(pub &'static str);

/// How a name matched: exactly, after folding case, or through an alias.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fold {
    /// The query was already the canonical spelling.
    Exact,
    /// The query differed only by case and was folded to the canonical spelling.
    CaseFolded,
    /// The query matched a declared alias.
    Alias { alias: String },
}

/// A catalogue item resolved through its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved<T> {
    /// The item the canonical name looks up.
    pub item: T,
    /// The canonical spelling the catalogue declares.
    pub canonical: String,
    /// How the query reached the canonical spelling.
    pub fold: Fold,
}

/// An unknown name, with the candidates worth trying.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameRefusal {
    /// What kind of name was requested.
    pub kind: NameKind,
    /// The name the caller supplied.
    pub query: String,
    /// Close matches, ranked, at most five.
    pub candidates: Vec<String>,
}

impl std::fmt::Display for NameRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "unknown {} `{}`", self.kind.0, self.query)?;
        if !self.candidates.is_empty() {
            write!(formatter, "; close matches: {}", self.candidates.join(", "))?;
        }
        Ok(())
    }
}

impl std::error::Error for NameRefusal {}

/// The names one family accepts and the item behind each canonical spelling.
pub trait Catalogue {
    /// The item a canonical name resolves to.
    type Item;
    /// What kind of name this catalogue holds.
    const KIND: NameKind;
    /// Every canonical spelling.
    fn names(&self) -> Vec<String>;
    /// The item for a canonical spelling, or `None` when the catalogue is
    /// inconsistent.
    fn lookup(&self, canonical: &str) -> Option<Self::Item>;
    /// Declared `(alias, canonical)` pairs.
    fn aliases(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}

/// Exact, then case-folded, then alias; else a refusal carrying close matches.
/// A fold other than [`Fold::Exact`] is the caller's cue to warn.
pub fn resolve<C: Catalogue>(catalogue: &C, query: &str) -> Result<Resolved<C::Item>, NameRefusal> {
    let names = catalogue.names();
    if let Some(canonical) = names.iter().find(|name| name.as_str() == query)
        && let Some(item) = catalogue.lookup(canonical)
    {
        return Ok(Resolved {
            item,
            canonical: canonical.clone(),
            fold: Fold::Exact,
        });
    }
    let folded = query.to_lowercase();
    if let Some(canonical) = names.iter().find(|name| name.to_lowercase() == folded)
        && let Some(item) = catalogue.lookup(canonical)
    {
        return Ok(Resolved {
            item,
            canonical: canonical.clone(),
            fold: Fold::CaseFolded,
        });
    }
    if let Some((alias, canonical)) = catalogue
        .aliases()
        .into_iter()
        .find(|(alias, _)| alias.as_str() == query)
        && let Some(item) = catalogue.lookup(&canonical)
    {
        return Ok(Resolved {
            item,
            canonical,
            fold: Fold::Alias { alias },
        });
    }
    Err(NameRefusal {
        kind: C::KIND,
        query: query.to_owned(),
        candidates: close_matches(query, names.iter().map(String::as_str))
            .into_iter()
            .map(str::to_owned)
            .collect(),
    })
}

/// Fold a user-facing name to its canonical spelling and say whether folding
/// changed it. Lower-case ASCII letters, digits, `-` and `_` are valid, and the
/// first character must be a letter or digit. `None` when the name is invalid.
pub fn valid_name(name: &str) -> Option<(String, bool)> {
    let mut characters = name.chars();
    let first = characters.next()?;
    if !first.is_ascii_alphanumeric() {
        return None;
    }
    let mut folded = String::with_capacity(name.len());
    let mut changed = false;
    for character in name.chars() {
        let canonical = character.to_ascii_lowercase();
        if !(canonical.is_ascii_lowercase()
            || canonical.is_ascii_digit()
            || canonical == '-'
            || canonical == '_')
        {
            return None;
        }
        changed |= canonical != character;
        folded.push(canonical);
    }
    Some((folded, changed))
}

#[cfg(test)]
mod tests {
    use super::{Catalogue, Fold, NameKind, close_matches, resolve, valid_name};
    use std::collections::BTreeMap;

    struct Names {
        items: BTreeMap<String, u32>,
        aliases: Vec<(String, String)>,
    }

    impl Catalogue for Names {
        type Item = u32;
        const KIND: NameKind = NameKind("model");

        fn names(&self) -> Vec<String> {
            self.items.keys().cloned().collect()
        }

        fn lookup(&self, canonical: &str) -> Option<Self::Item> {
            self.items.get(canonical).copied()
        }

        fn aliases(&self) -> Vec<(String, String)> {
            self.aliases.clone()
        }
    }

    #[test]
    fn a_one_edit_typo_ranks_first_and_an_unrelated_name_has_no_matches() {
        let matches = close_matches("writter", ["reader", "writer", "router"]);
        assert_eq!(matches.first(), Some(&"writer"));
        assert!(close_matches("zzzzzz", ["reader", "writer"]).is_empty());
    }

    #[test]
    fn a_case_fold_is_reported_and_an_exact_name_is_not() {
        let catalogue = Names {
            items: BTreeMap::from([("writer".to_owned(), 1)]),
            aliases: Vec::new(),
        };
        let folded = resolve(&catalogue, "Writer").expect("folded");
        assert_eq!(folded.canonical, "writer");
        assert_eq!(folded.fold, Fold::CaseFolded);
        assert_eq!(folded.item, 1);
        assert_eq!(
            resolve(&catalogue, "writer").expect("exact").fold,
            Fold::Exact
        );
    }

    #[test]
    fn an_alias_names_itself_in_the_fold() {
        let catalogue = Names {
            items: BTreeMap::from([("writer".to_owned(), 1)]),
            aliases: vec![("w".to_owned(), "writer".to_owned())],
        };
        let resolved = resolve(&catalogue, "w").expect("alias");
        assert_eq!(
            resolved.fold,
            Fold::Alias {
                alias: "w".to_owned()
            }
        );
        assert_eq!(resolved.canonical, "writer");
    }

    #[test]
    fn an_unknown_name_refuses_with_close_matches() {
        let catalogue = Names {
            items: BTreeMap::from([("writer".to_owned(), 1), ("reviewer".to_owned(), 2)]),
            aliases: Vec::new(),
        };
        let refusal = resolve(&catalogue, "writter").expect_err("unknown");
        assert_eq!(refusal.kind, NameKind("model"));
        assert!(refusal.candidates.contains(&"writer".to_owned()));
        let message = refusal.to_string();
        assert!(message.contains("unknown model `writter`"), "{message}");
        assert!(message.contains("close matches:"), "{message}");
    }

    #[test]
    fn a_valid_name_folds_case_and_refuses_the_rest() {
        assert_eq!(valid_name("Writer_2"), Some(("writer_2".to_owned(), true)));
        assert_eq!(valid_name("writer-2"), Some(("writer-2".to_owned(), false)));
        for invalid in ["", "-writer", "_writer", "writ er", "writé", "write/err"] {
            assert_eq!(valid_name(invalid), None, "{invalid:?}");
        }
    }
}
