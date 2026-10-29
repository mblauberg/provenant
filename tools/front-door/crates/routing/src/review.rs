use crate::{Catalogue, RoutingConfig, request::identifier};
use provenant_config::SectionError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReviewConfig {
    pub shapes: BTreeMap<String, ReviewShape>,
    pub select: ReviewSelect,
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
    }
    #[derive(Deserialize)]
    struct SelectAsset {
        default: String,
        by_risk: BTreeMap<String, String>,
        by_kind: BTreeMap<String, String>,
        by_paths: BTreeMap<String, String>,
    }
    let asset: Asset = toml_edit::de::from_str(include_str!("../assets/review-defaults.toml"))
        .unwrap_or_else(|error| panic!("invalid bundled review defaults: {error}"));
    let select = asset.review.select;
    ReviewConfig {
        shapes: asset.review.shapes,
        select: ReviewSelect {
            default: select.default,
            by_risk: select.by_risk,
            by_kind: select.by_kind,
            by_paths: select.by_paths,
            by_size: BTreeMap::new(),
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
    ) -> Result<ReviewSelection<'a>, String> {
        let selected = self.select.select(
            override_name.map(str::to_owned).as_ref(),
            risk,
            kind,
            paths,
            None,
            Some(true),
        );
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
                .select_shape(shape, risk, kind, paths)
                .unwrap()
                .source
        };
        assert_eq!(
            select(Some("quick"), Some("low"), Some("docs"), &paths),
            "override"
        );
        assert_eq!(select(None, Some("low"), Some("docs"), &paths), "by_paths");
        assert_eq!(select(None, Some("low"), Some("docs"), &[]), "by_risk");
        assert_eq!(select(None, None, Some("docs"), &[]), "by_kind");
        assert_eq!(select(None, None, None, &[]), "default");
        assert!(
            review
                .select_shape(Some("missing"), None, None, &[])
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
            review.select_shape(None, None, None, &paths).unwrap().name,
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
        assert_eq!(config.review.shapes["deep"].reviewers[0].lenses.len(), 4);
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
