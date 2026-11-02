use crate::{Catalogue, RoutingConfig, request::identifier};
use provenant_config::SectionError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReviewConfig {
    pub shapes: BTreeMap<String, ReviewShape>,
    pub select: ReviewSelect,
    pub size: ReviewSize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReviewSize {
    pub small_lines: usize,
    pub small_files: usize,
    pub small_crates: usize,
    pub medium_lines: usize,
    pub medium_files: usize,
    pub medium_crates: usize,
}
impl Default for ReviewSize {
    fn default() -> Self {
        Self {
            small_lines: 150,
            small_files: 3,
            small_crates: 1,
            medium_lines: 1000,
            medium_files: 15,
            medium_crates: 3,
        }
    }
}
impl ReviewSize {
    pub fn classify(&self, lines: usize, files: usize, crates: usize) -> &'static str {
        if lines <= self.small_lines && files <= self.small_files && crates <= self.small_crates {
            "small"
        } else if lines <= self.medium_lines
            && files <= self.medium_files
            && crates <= self.medium_crates
        {
            "medium"
        } else {
            "large"
        }
    }

    /// Classify NUL-delimited Git numstat and tree paths. Binary changes have
    /// no line count, so conservatively classify them as large.
    pub fn classify_numstat(&self, numstat: &str, tree: &str) -> &'static str {
        let mut lines = 0usize;
        let mut files = 0usize;
        let mut crates = std::collections::BTreeSet::new();
        let roots: Vec<&str> = tree
            .split('\0')
            .filter_map(|path| {
                if path == "Cargo.toml" {
                    Some("")
                } else {
                    path.strip_suffix("/Cargo.toml")
                }
            })
            .collect();
        for row in numstat.split('\0').filter(|row| !row.is_empty()) {
            let mut fields = row.splitn(3, '\t');
            let (Some(added), Some(removed), Some(path)) =
                (fields.next(), fields.next(), fields.next())
            else {
                return "large";
            };
            let (Ok(added), Ok(removed)) = (added.parse::<usize>(), removed.parse::<usize>())
            else {
                return "large";
            };
            lines = lines.saturating_add(added).saturating_add(removed);
            files += 1;
            if let Some(root) = roots
                .iter()
                .filter(|root| {
                    root.is_empty()
                        || path
                            .strip_prefix(**root)
                            .is_some_and(|suffix| suffix.starts_with('/'))
                })
                .max_by_key(|root| root.len())
            {
                crates.insert(*root);
            }
        }
        self.classify(lines, files, crates.len())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewShape {
    pub reviewers: Vec<ReviewReviewer>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewReviewer {
    pub pool: String,
    pub count: usize,
    #[serde(default)]
    pub lenses: Vec<String>,
    #[serde(default)]
    pub distinct_families: bool,
}
/// Declarative selector shared by review shapes and pipeline presets.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Select<T> {
    pub default: T,
    pub by_risk: BTreeMap<String, T>,
    pub by_kind: BTreeMap<String, T>,
    pub by_paths: BTreeMap<String, T>,
    pub by_size: BTreeMap<String, T>,
    pub by_readiness: BTreeMap<String, T>,
}
impl<T: Default> Default for Select<T> {
    fn default() -> Self {
        Self {
            default: T::default(),
            by_risk: BTreeMap::new(),
            by_kind: BTreeMap::new(),
            by_paths: BTreeMap::new(),
            by_size: BTreeMap::new(),
            by_readiness: BTreeMap::new(),
        }
    }
}
pub type ReviewSelect = Select<String>;
/// Base lens order. Conditional risk lenses are selected separately.
pub fn lens_priority(kind: Option<&str>) -> &'static [&'static str] {
    match kind {
        Some("docs") => &["conventions-readability"],
        Some("refactor") => &[
            "correctness-spec",
            "refactor-cleanup",
            "design-fit-reuse",
            "tests",
            "simplicity",
            "conventions-readability",
            "reliability",
            "flexibility",
        ],
        _ => &[
            "correctness-spec",
            "tests",
            "design-fit-reuse",
            "simplicity",
            "conventions-readability",
            "refactor-cleanup",
            "reliability",
            "flexibility",
        ],
    }
}
impl Default for ReviewConfig {
    fn default() -> Self {
        bundled()
    }
}
#[allow(clippy::panic)]
fn bundled() -> ReviewConfig {
    #[derive(Deserialize)]
    struct Asset {
        review: ReviewAsset,
    }
    #[derive(Deserialize)]
    struct ReviewAsset {
        shapes: BTreeMap<String, ReviewShape>,
        select: SelectAsset,
        size: ReviewSize,
    }
    #[derive(Deserialize)]
    struct SelectAsset {
        default: String,
        by_risk: BTreeMap<String, String>,
        by_kind: BTreeMap<String, String>,
        by_paths: BTreeMap<String, String>,
        by_size: BTreeMap<String, String>,
    }
    let asset: Asset = toml_edit::de::from_str(include_str!("../assets/review-defaults.toml"))
        .unwrap_or_else(|error| panic!("invalid bundled review defaults: {error}"));
    let select = asset.review.select;
    ReviewConfig {
        shapes: asset.review.shapes,
        size: asset.review.size,
        select: ReviewSelect {
            default: select.default,
            by_risk: select.by_risk,
            by_kind: select.by_kind,
            by_paths: select.by_paths,
            by_size: select.by_size,
            by_readiness: BTreeMap::new(),
        },
    }
}
pub struct ReviewSelection<'a> {
    pub name: &'a str,
    pub source: &'static str,
    pub shape: &'a ReviewShape,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection<T> {
    pub value: T,
    pub source: &'static str,
    pub matched: Option<String>,
}
impl<T: Clone> Select<T> {
    pub fn select(
        &self,
        override_value: Option<&T>,
        risk: Option<&str>,
        kind: Option<&str>,
        paths: &[String],
        size: Option<&str>,
        ready: Option<bool>,
    ) -> Selection<T> {
        let selected = if let Some(value) = override_value {
            (value, "override", None)
        } else if let Some(ready) = ready
            && let Some(value) =
                self.by_readiness
                    .get(if ready { "ready" } else { "underspecified" })
        {
            (
                value,
                "readiness",
                Some(format!(
                    "readiness:{}",
                    if ready { "ready" } else { "underspecified" }
                )),
            )
        } else if let Some((pattern, value)) = self
            .by_paths
            .iter()
            .filter(|(pattern, _)| paths.iter().any(|path| path_matches(pattern, path)))
            .max_by(|(a, _), (b, _)| {
                path_specificity(a)
                    .cmp(&path_specificity(b))
                    .then_with(|| b.cmp(a))
            })
        {
            (value, "by_paths", Some(format!("by_paths:{pattern}")))
        } else if let Some((key, value)) = risk.and_then(|key| self.by_risk.get_key_value(key)) {
            (value, "by_risk", Some(format!("by_risk:{key}")))
        } else if let Some((key, value)) = kind.and_then(|key| self.by_kind.get_key_value(key)) {
            (value, "by_kind", Some(format!("by_kind:{key}")))
        } else if let Some((key, value)) = size.and_then(|key| self.by_size.get_key_value(key)) {
            (value, "by_size", Some(format!("by_size:{key}")))
        } else {
            (&self.default, "default", None)
        };
        Selection {
            value: selected.0.clone(),
            source: selected.1,
            matched: selected.2,
        }
    }
}
impl ReviewConfig {
    pub fn select_shape<'a>(
        &'a self,
        override_name: Option<&'a str>,
        risk: Option<&str>,
        kind: Option<&str>,
        paths: &[String],
        size: Option<&str>,
    ) -> Result<ReviewSelection<'a>, String> {
        let integration = (kind == Some("integration"))
            .then(|| self.select.by_kind.get("integration"))
            .flatten();
        let mut selected = self.select.select(
            integration.or(override_name.map(str::to_owned).as_ref()),
            risk,
            kind,
            paths,
            size,
            Some(true),
        );
        if integration.is_some() {
            selected.source = "by_kind";
        }
        // Risk establishes a floor; size and integration can demand more coverage.
        {
            let candidates = [
                (override_name.is_none() && (kind.is_some_and(|k| self.select.by_kind.contains_key(k))
                    || size.is_some_and(|s| self.select.by_size.contains_key(s)))
                )
                .then(|| self.select.select(None, None, kind, &[], size, Some(true))),
                risk.filter(|r| self.select.by_risk.contains_key(*r))
                    .map(|r| {
                        self.select
                            .select(None, Some(r), None, &[], None, Some(true))
                    }),
            ];
            for candidate in candidates.into_iter().flatten() {
                let slots = |name: &str| {
                    self.shapes.get(name).map_or(0, |shape| {
                        shape.reviewers.iter().map(|r| r.count).sum::<usize>()
                    })
                };
                if slots(&candidate.value) > slots(&selected.value) {
                    selected = candidate;
                }
            }
        }
        let shape = self
            .shapes
            .get(&selected.value)
            .ok_or_else(|| format!("unknown review shape '{}'", selected.value))?;
        Ok(ReviewSelection {
            name: self
                .shapes
                .get_key_value(&selected.value)
                .map_or("", |(name, _)| name),
            source: selected.source,
            shape,
        })
    }
    pub(crate) fn validate(
        &self,
        config: &RoutingConfig,
        catalogue: Option<&Catalogue>,
        errors: &mut Vec<SectionError>,
    ) {
        let mut error = |field: String, code| errors.push(SectionError { field, code });
        if self.shapes.len() > 128 {
            error("review.shapes".into(), "limit");
        }
        for (name, shape) in &self.shapes {
            let prefix = format!("review.shapes.{name}");
            if !identifier(name) {
                error(prefix.clone(), "selector");
            }
            if shape.reviewers.is_empty() || shape.reviewers.len() > 32 {
                error(format!("{prefix}.reviewers"), "limit");
            }
            if shape
                .reviewers
                .iter()
                .fold(0_usize, |total, row| total.saturating_add(row.count))
                > 64
            {
                error(format!("{prefix}.reviewers"), "limit");
            }
            for (index, reviewer) in shape.reviewers.iter().enumerate() {
                let prefix = format!("{prefix}.reviewers[{index}]");
                if !config.pools.contains_key(&reviewer.pool)
                    && !catalogue.is_some_and(|c| c.has_pool(&reviewer.pool))
                {
                    error(format!("{prefix}.pool"), "unknown_pool");
                }
                if reviewer.count == 0 || reviewer.count > 32 {
                    error(format!("{prefix}.count"), "range");
                }
                if (!reviewer.lenses.is_empty() && reviewer.lenses.len() != reviewer.count)
                    || reviewer.lenses.iter().any(|lens| !identifier(lens))
                {
                    error(format!("{prefix}.lenses"), "invalid_lenses");
                }
            }
        }
        if !self.shapes.contains_key(&self.select.default) {
            error("review.select.default".into(), "unknown_shape");
        }
        for (field, map) in [
            ("by_risk", &self.select.by_risk),
            ("by_kind", &self.select.by_kind),
            ("by_paths", &self.select.by_paths),
            ("by_size", &self.select.by_size),
        ] {
            for (key, shape) in map {
                if (field != "by_paths" && !identifier(key))
                    || (field == "by_paths" && (key.is_empty() || key.len() > 512))
                {
                    error(format!("review.select.{field}.{key}"), "selector");
                }
                if !self.shapes.contains_key(shape) {
                    error(format!("review.select.{field}.{key}"), "unknown_shape");
                }
            }
        }
    }
}
// Prefer patterns with more literal bytes, then fewer wildcards. Lexical order
// breaks ties, independently of the order in which config layers were merged.
fn path_specificity(pattern: &str) -> (usize, std::cmp::Reverse<usize>) {
    let wildcards = pattern
        .bytes()
        .filter(|byte| matches!(byte, b'*' | b'?'))
        .count();
    (pattern.len() - wildcards, std::cmp::Reverse(wildcards))
}
// * and ? stay within one path component; ** also matches slashes.
fn path_matches(pattern: &str, path: &str) -> bool {
    fn go(
        p: &[u8],
        s: &[u8],
        memo: &mut BTreeMap<(usize, usize), bool>,
        plen: usize,
        slen: usize,
    ) -> bool {
        let key = (plen - p.len(), slen - s.len());
        if let Some(result) = memo.get(&key) {
            return *result;
        }
        let result = match p.first() {
            None => s.is_empty(),
            Some(b'*') => {
                let double = p.get(1) == Some(&b'*');
                let rest = &p[if double { 2 } else { 1 }..];
                go(rest, s, memo, plen, slen)
                    || (double
                        && rest.first() == Some(&b'/')
                        && go(&rest[1..], s, memo, plen, slen))
                    || (!s.is_empty()
                        && (double || s[0] != b'/')
                        && go(p, &s[1..], memo, plen, slen))
            }
            Some(b'?') => !s.is_empty() && s[0] != b'/' && go(&p[1..], &s[1..], memo, plen, slen),
            Some(c) => s.first() == Some(c) && go(&p[1..], &s[1..], memo, plen, slen),
        };
        memo.insert(key, result);
        result
    }
    go(
        pattern.as_bytes(),
        path.as_bytes(),
        &mut BTreeMap::new(),
        pattern.len(),
        path.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_config::DomainConfig;

    #[test]
    fn size_and_kind_choose_shapes_with_risk_floor() {
        let review = ReviewConfig::default();
        for (lines, files, crates, expected) in [
            (150, 3, 1, "quick"),
            (151, 3, 1, "standard"),
            (1001, 3, 1, "deep"),
            (1, 16, 1, "deep"),
            (1, 1, 4, "deep"),
        ] {
            let size = review.size.classify(lines, files, crates);
            assert_eq!(
                review
                    .select_shape(None, None, None, &[], Some(size))
                    .unwrap()
                    .name,
                expected
            );
        }
        assert_eq!(
            review
                .select_shape(
                    None,
                    Some("routine"),
                    Some("integration"),
                    &[],
                    Some("small")
                )
                .unwrap()
                .name,
            "deep"
        );
        assert_eq!(
            review
                .select_shape(None, None, Some("docs"), &[], Some("large"))
                .unwrap()
                .name,
            "quick"
        );
        assert_eq!(
            review
                .select_shape(None, None, Some("refactor"), &[], Some("medium"))
                .unwrap()
                .name,
            "standard"
        );
        assert_eq!(
            review
                .select_shape(None, Some("crucial"), None, &[], Some("small"))
                .unwrap()
                .name,
            "deep"
        );
        assert_eq!(review.select_shape(Some("quick"), Some("crucial"), None, &[], None).unwrap().name, "deep");
    }

    #[test]
    fn numstat_counts_manifest_roots_and_binary_changes() {
        let size = ReviewSize::default();
        let trees = "Cargo.toml\0apps/a/Cargo.toml\0apps/b/Cargo.toml\0";
        assert_eq!(
            size.classify_numstat("20\t3\tapps/a/src/main.rs\0", trees),
            "small"
        );
        assert_eq!(
            size.classify_numstat(
                "20\t3\tapps/a/src/main.rs\01\t1\tapps/b/src/lib.rs\0",
                trees
            ),
            "medium"
        );
        assert_eq!(size.classify_numstat("-\t-\timage.png\0", trees), "large");
    }

    #[test]
    fn bundled_review_shapes_cover_all_risk_tiers() {
        let review = ReviewConfig::default();
        for (tier, expected) in [
            ("routine", "quick"),
            ("substantial", "standard"),
            ("crucial", "deep"),
            ("terminal", "deep"),
        ] {
            let selected = review
                .select_shape(None, Some(tier), None, &[], None)
                .unwrap();
            assert_eq!(selected.name, expected, "risk tier {tier}");
            assert_eq!(selected.source, "by_risk", "risk tier {tier}");
        }
    }

    #[test]
    fn selection_precedence_and_path_globs() {
        let mut review = ReviewConfig::default();
        review
            .select
            .by_paths
            .insert(".github/workflows/**".into(), "deep".into());
        review.select.by_kind.insert("docs".into(), "quick".into());
        let paths = vec![".github/workflows/nested/test.yml".into()];
        let select = |shape, risk, kind, paths: &[String]| {
            review
                .select_shape(shape, risk, kind, paths, None)
                .unwrap()
                .source
        };
        assert_eq!(
            select(Some("quick"), Some("routine"), Some("docs"), &paths),
            "override"
        );
        assert_eq!(
            select(None, Some("routine"), Some("docs"), &paths),
            "by_paths"
        );
        assert_eq!(select(None, Some("routine"), Some("docs"), &[]), "by_risk");
        assert_eq!(select(None, None, Some("docs"), &[]), "by_kind");
        assert_eq!(select(None, None, None, &[]), "default");
        assert!(
            review
                .select_shape(Some("missing"), None, None, &[], None)
                .is_err()
        );
        assert!(path_matches("src/**/test?.rs", "src/test1.rs"));
        assert!(path_matches("src/**/test?.rs", "src/a/b/test1.rs"));
        assert!(!path_matches("src/*.rs", "src/a/test.rs"));
        review
            .select
            .by_paths
            .insert("src/**".into(), "standard".into());
        review
            .select
            .by_paths
            .insert("src/auth/**".into(), "deep".into());
        let paths = vec!["src/auth/login.rs".into()];
        assert_eq!(
            review
                .select_shape(None, None, None, &paths, None)
                .unwrap()
                .name,
            "deep"
        );
    }

    #[test]
    fn generic_select_supports_size_and_readiness_rules() {
        let select = Select {
            default: "single".to_owned(),
            by_risk: BTreeMap::from([("critical".into(), "review-council".into())]),
            by_kind: BTreeMap::from([("docs".into(), "chain".into())]),
            by_paths: BTreeMap::from([("src/**".into(), "fan-out".into())]),
            by_size: BTreeMap::from([("large".into(), "big-batch".into())]),
            by_readiness: BTreeMap::from([("underspecified".into(), "scope-then-run".into())]),
        };
        let paths = vec!["src/lib.rs".to_owned()];
        assert_eq!(
            select
                .select(
                    None,
                    Some("critical"),
                    Some("docs"),
                    &paths,
                    Some("large"),
                    Some(false)
                )
                .value,
            "scope-then-run"
        );
        assert_eq!(
            select
                .select(
                    None,
                    Some("critical"),
                    Some("docs"),
                    &paths,
                    Some("large"),
                    Some(true)
                )
                .value,
            "fan-out"
        );
        assert_eq!(
            select
                .select(None, None, None, &[], Some("large"), Some(true))
                .value,
            "big-batch"
        );
        assert_eq!(
            select
                .select(
                    None,
                    Some("critical"),
                    Some("docs"),
                    &[],
                    Some("large"),
                    Some(true)
                )
                .value,
            "review-council"
        );
        assert_eq!(
            select
                .select(None, None, Some("docs"), &[], Some("large"), Some(true))
                .value,
            "chain"
        );
        assert_eq!(
            select
                .select(None, None, None, &[], None, Some(true))
                .source,
            "default"
        );
    }

    #[test]
    fn parsing_defaults_and_validation() {
        let config: RoutingConfig =
            toml_edit::de::from_str(include_str!("../assets/review-defaults.toml")).unwrap();
        assert!(config.review.shapes["deep"].reviewers[0].lenses.is_empty());
        assert!(config.validate().is_empty());
        let mut invalid = config;
        invalid.review.shapes.get_mut("quick").unwrap().reviewers[0].pool = "missing".into();
        invalid.review.shapes.get_mut("quick").unwrap().reviewers[0].lenses =
            vec!["bad lens".into()];
        let errors = invalid.validate();
        assert!(errors.iter().any(|e| e.code == "unknown_pool"));
        assert!(errors.iter().any(|e| e.code == "invalid_lenses"));
        invalid.review.shapes.get_mut("standard").unwrap().reviewers = vec![
            ReviewReviewer {
                pool: "junior".into(),
                count: 32,
                lenses: vec![],
                distinct_families: false,
            },
            ReviewReviewer {
                pool: "junior".into(),
                count: 32,
                lenses: vec![],
                distinct_families: false,
            },
            ReviewReviewer {
                pool: "junior".into(),
                count: 1,
                lenses: vec![],
                distinct_families: false,
            },
        ];
        assert!(
            invalid
                .validate()
                .iter()
                .any(|e| e.field == "review.shapes.standard.reviewers" && e.code == "limit")
        );
    }
}
