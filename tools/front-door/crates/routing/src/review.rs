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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReviewSelect {
    pub default: String,
    pub by_risk: BTreeMap<String, String>,
    pub by_kind: BTreeMap<String, String>,
    pub by_paths: BTreeMap<String, String>,
}
impl Default for ReviewSelect {
    fn default() -> Self {
        bundled().select
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
        },
    }
}
pub struct ReviewSelection<'a> {
    pub name: &'a str,
    pub source: &'static str,
    pub shape: &'a ReviewShape,
}
impl ReviewConfig {
    pub fn select_shape<'a>(
        &'a self,
        override_name: Option<&'a str>,
        risk: Option<&str>,
        kind: Option<&str>,
        paths: &[String],
    ) -> Result<ReviewSelection<'a>, String> {
        let selected = if let Some(name) = override_name {
            (name, "override")
        } else if let Some((_, name)) = self
            .select
            .by_paths
            .iter()
            .filter(|(pattern, _)| paths.iter().any(|path| path_matches(pattern, path)))
            .max_by(|(a, _), (b, _)| {
                path_specificity(a)
                    .cmp(&path_specificity(b))
                    .then_with(|| b.cmp(a))
            })
        {
            (name.as_str(), "by_paths")
        } else if let Some(name) = risk.and_then(|r| self.select.by_risk.get(r)) {
            (name.as_str(), "by_risk")
        } else if let Some(name) = kind.and_then(|k| self.select.by_kind.get(k)) {
            (name.as_str(), "by_kind")
        } else {
            (self.select.default.as_str(), "default")
        };
        let shape = self
            .shapes
            .get(selected.0)
            .ok_or_else(|| format!("unknown review shape '{}'", selected.0))?;
        Ok(ReviewSelection {
            name: selected.0,
            source: selected.1,
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
