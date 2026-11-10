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
    pub lenses: ReviewLenses,
    /// Configurable path classes and their minimum review coverage.
    pub floors: BTreeMap<String, ReviewFloor>,
    /// Whether an unmet panel family minimum refuses the review.
    pub diversity: FamilyDiversity,
}

/// How a shortfall against a panel's `minimum_family_counts` is handled.
/// Advisory resolves the panel and records a warning; strict refuses it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FamilyDiversity {
    #[default]
    Advisory,
    Strict,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReviewFloor {
    pub paths: Vec<String>,
    pub tier: String,
    pub lenses: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppliedFloors {
    pub classes: Vec<String>,
    pub tier: Option<String>,
    pub lenses: Vec<String>,
}

/// Which lenses fill a panel. `order` ranks the base lenses that fill the
/// shape's slots; a kind may replace that order (`docs`) or add lenses outside
/// the slot cap (`refactor`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReviewLenses {
    pub order: Vec<String>,
    pub by_kind: BTreeMap<String, KindLenses>,
    pub by_paths: BTreeMap<String, PathLenses>,
}
impl Default for ReviewLenses {
    fn default() -> Self {
        bundled().lenses
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KindLenses {
    /// Replaces the base order for this kind when nonempty.
    pub order: Vec<String>,
    /// Lenses added outside the shape's slot cap.
    pub add: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PathLenses {
    /// Lenses added outside the shape's slot cap when a changed path matches.
    pub add: Vec<String>,
}
/// The lens order, exclusivity and additions that apply to one change kind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LensPlan {
    pub order: Vec<String>,
    /// The kind names its own base order, so other lenses are not base lenses.
    pub exclusive: bool,
    pub add: Vec<String>,
    /// The part of `add` that the change itself triggers (matching path rules
    /// and test markers). A caller-supplied lens list still takes these.
    pub change_add: Vec<String>,
    /// Matching path rules that added lenses, in config key order.
    pub matched_paths: Vec<String>,
    /// Whether changed diff hunks contain a recognised test marker.
    pub test_marker: bool,
}
/// Conditional risk tags and the lens each one adds. The single source for the
/// CLI flag values, the native panel and the council's `when` triggers.
pub const RISK_TAG_LENSES: [(&str, &str); 3] = [
    ("security", "security-privacy"),
    ("concurrency", "concurrency-data-errors"),
    ("terminal", "adversarial"),
];
pub fn risk_tag_names() -> [&'static str; 3] {
    RISK_TAG_LENSES.map(|(tag, _)| tag)
}
/// Lenses triggered by risk tags. A terminal risk tier implies the terminal tag.
pub fn risk_lenses<S: AsRef<str>>(tags: &[S], risk: Option<&str>) -> Vec<&'static str> {
    RISK_TAG_LENSES
        .iter()
        .filter(|(tag, _)| {
            tags.iter().any(|value| value.as_ref() == *tag)
                || (*tag == "terminal" && risk == Some("terminal"))
        })
        .map(|(_, lens)| *lens)
        .collect()
}
/// Whether a lens tier value names a route: a pool name or exact model (a
/// nonempty string), a route table (a nonempty object) or a route list (a
/// nonempty array). Anything else leaves the tier to the review shape.
pub fn tier_is_set(tier: &serde_json::Value) -> bool {
    match tier {
        serde_json::Value::String(text) => !text.is_empty(),
        serde_json::Value::Object(table) => !table.is_empty(),
        serde_json::Value::Array(list) => !list.is_empty(),
        _ => false,
    }
}

/// A lens candidate in a review request. An unset `tier` takes the review
/// shape's pool; a set one (pool, exact `ADAPTER/ID[@effort]` or route table)
/// is kept as given.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewLens {
    pub name: String,
    /// Hard provider-family requirement, enforced when eligible routes are known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_family: Option<String>,
    /// The risk tag that must be present for the lens to run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub tier: serde_json::Value,
}

/// What a review asks of panel resolution. An empty `lenses` list takes the
/// configured lens order and risk-tag lenses.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRequest {
    /// Minimum number of selected lenses assigned to each provider family.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub minimum_family_counts: BTreeMap<String, usize>,
    /// Explicit shape; the risk floor can still raise it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Explicit size override; it wins over the measured size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risk_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lenses: Vec<ReviewLens>,
    /// Overrides the configured `review.diversity` for this review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diversity: Option<FamilyDiversity>,
}

/// One resolved reviewer slot in the panel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedSlot {
    pub lens: String,
    /// Actual family selected to satisfy a hard panel or lens constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub satisfied_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_family: Option<String>,
    /// A pool name, an exact `ADAPTER/ID[@effort]` or a route table, as given
    /// or taken from the shape.
    pub tier: serde_json::Value,
    /// The shape reviewer group the slot came from.
    pub group_index: usize,
    /// A preference that sibling slots in the group use different model
    /// families. The scheduler substitutes and records when it cannot; it
    /// never drops the lens or fails the review for it.
    pub distinct_families: bool,
}

/// A lens that was omitted from the panel, with the reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmittedLens {
    pub lens: String,
    pub reason: String,
}

/// The result of resolving a review panel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelResolution {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub minimum_family_counts: BTreeMap<String, usize>,
    pub shape: String,
    pub source: String,
    pub frame: bool,
    pub verifiers: usize,
    pub synthesis: ReviewSynthesis,
    pub slots: Vec<ResolvedSlot>,
    pub omitted: Vec<OmittedLens>,
    #[serde(default, skip_serializing_if = "is_advisory")]
    pub diversity: FamilyDiversity,
    /// Family minimums the panel could not meet under advisory diversity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

fn is_advisory(diversity: &FamilyDiversity) -> bool {
    *diversity == FamilyDiversity::Advisory
}

impl PanelResolution {
    /// Assign hard family requirements using the ordinary eligible route resolver.
    /// The callback probes a slot without launching it. Unconstrained panels do
    /// not probe or alter any route, preserving ordinary pool selection.
    pub fn resolve_families(
        &mut self,
        mut eligible: impl FnMut(&ResolvedSlot, Option<&str>) -> Result<String, String>,
    ) -> Result<(), String> {
        if self.minimum_family_counts.is_empty()
            && self.slots.iter().all(|slot| slot.required_family.is_none())
        {
            return Ok(());
        }
        for slot in &mut self.slots {
            slot.satisfied_family = None;
        }
        self.warnings.clear();
        let strict = self.diversity == FamilyDiversity::Strict;
        let mut warnings = Vec::new();
        let mut selected = vec![None; self.slots.len()];
        let mut fixed_counts = BTreeMap::<String, usize>::new();
        for (index, slot) in self.slots.iter().enumerate() {
            if let Some(family) = &slot.required_family {
                let actual = eligible(slot, Some(family)).map_err(|reason| {
                    format!("required provider family {family}: count 1 for lens {} cannot be met: {reason}", slot.lens)
                })?;
                if actual != *family {
                    return Err(format!(
                        "required provider family {family}: count 1 for lens {} cannot be met: resolver returned family {actual}",
                        slot.lens
                    ));
                }
                selected[index] = Some(family.clone());
                *fixed_counts.entry(family.clone()).or_default() += 1;
            }
        }
        let mut demands = Vec::new();
        let mut edges = Vec::new();
        let mut exclusions = Vec::new();
        for (family, count) in &self.minimum_family_counts {
            let remaining = count.saturating_sub(*fixed_counts.get(family).unwrap_or(&0));
            if remaining == 0 {
                continue;
            }
            let mut candidates = Vec::new();
            let mut reasons = Vec::new();
            for (index, slot) in self.slots.iter().enumerate() {
                if let Some(fixed) = &selected[index] {
                    reasons.push(format!("{}: requires family {fixed}", slot.lens));
                    continue;
                }
                match eligible(slot, Some(family)) {
                    Ok(actual) if actual == *family => candidates.push(index),
                    Ok(actual) => {
                        reasons.push(format!("{}: resolver returned family {actual}", slot.lens))
                    }
                    Err(reason) => reasons.push(format!("{}: {reason}", slot.lens)),
                }
            }
            let room = self.slots.len().saturating_sub(demands.len());
            if remaining > room {
                let message = format!(
                    "required provider family {family}: count {count} cannot be met by {} lenses; exclusions: {}",
                    self.slots.len(),
                    reasons.join("; ")
                );
                if strict {
                    return Err(message);
                }
                warnings.push(message);
            }
            for _ in 0..remaining.min(room) {
                demands.push(family.clone());
                edges.push(candidates.clone());
                exclusions.push(reasons.clone());
            }
        }
        fn assign(
            demand: usize,
            edges: &[Vec<usize>],
            owners: &mut [Option<usize>],
            seen: &mut [bool],
        ) -> bool {
            for &slot in &edges[demand] {
                if seen[slot] {
                    continue;
                }
                seen[slot] = true;
                if owners[slot].is_none_or(|previous| assign(previous, edges, owners, seen)) {
                    owners[slot] = Some(demand);
                    return true;
                }
            }
            false
        }
        let mut owners = vec![None; self.slots.len()];
        for (demand, family) in demands.iter().enumerate() {
            if !assign(
                demand,
                &edges,
                &mut owners,
                &mut vec![false; self.slots.len()],
            ) {
                let count = self.minimum_family_counts[family];
                let reasons = if exclusions[demand].is_empty() {
                    "eligible lenses are needed by other required family counts".to_owned()
                } else {
                    exclusions[demand].join("; ")
                };
                let message = format!(
                    "required provider family {family}: count {count} cannot be met; exclusions: {reasons}"
                );
                if strict {
                    return Err(message);
                }
                warnings.push(message);
            }
        }
        for (index, owner) in owners.iter().enumerate() {
            if let Some(demand) = owner {
                selected[index] = Some(demands[*demand].clone());
            }
        }
        for (slot, family) in self.slots.iter_mut().zip(selected) {
            slot.satisfied_family = family;
        }
        self.warnings = warnings;
        Ok(())
    }
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
/// Outcome of measuring a diff: a size, or unknown with the reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Measured {
    pub size: Option<&'static str>,
    pub warning: Option<String>,
}
impl ReviewSize {
    /// Measure the change from the merge base of `base` and `head` to `head`.
    /// `git` runs one Git command and returns its stdout, or an error that
    /// carries Git's stderr. A failure degrades to an unknown size with a
    /// warning and never an error, so review selection always proceeds.
    /// Integration heads are always reviewed in full and are not measured, and
    /// an empty diff leaves the size unknown because the subject may not exist yet.
    pub fn measure(
        &self,
        kind: Option<&str>,
        base: &str,
        head: &str,
        git: &mut dyn FnMut(&[&str]) -> Result<String, String>,
    ) -> Measured {
        if kind == Some("integration") {
            return Measured::default();
        }
        let unknown = |reason: String| Measured {
            size: None,
            warning: Some(format!(
                "cannot measure review size {base}..{head}: {reason}; size unknown"
            )),
        };
        if base.starts_with('-') || head.starts_with('-') {
            return unknown("refs must not begin with -".into());
        }
        let merge_base = match git(&["merge-base", base, head]) {
            Ok(out) => out.trim().to_owned(),
            Err(error) => return unknown(error),
        };
        let numstat = match git(&[
            "diff",
            "--numstat",
            "--no-renames",
            "-z",
            &merge_base,
            head,
            "--",
        ]) {
            Ok(out) => out,
            Err(error) => return unknown(error),
        };
        if numstat.is_empty() {
            return Measured::default();
        }
        let mut tree = String::new();
        for rev in [merge_base.as_str(), head] {
            match git(&["ls-tree", "-r", "-z", "--name-only", rev]) {
                Ok(out) => tree.push_str(&out),
                Err(error) => return unknown(error),
            }
        }
        Measured {
            size: Some(self.classify_numstat(&numstat, &tree)),
            warning: None,
        }
    }

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
    /// Optional hard family floors, applied to the selected lenses.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub minimum_family_counts: BTreeMap<String, usize>,
    pub reviewers: Vec<ReviewReviewer>,
    #[serde(default = "default_frame")]
    pub frame: bool,
    #[serde(default = "default_verifiers")]
    pub verifiers: usize,
    #[serde(default)]
    pub synthesis: ReviewSynthesis,
}
fn default_frame() -> bool {
    true
}
fn default_verifiers() -> usize {
    1
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSynthesis {
    #[default]
    Model,
    Reduce,
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
/// The bundled defaults, parsed once per process. Every deserialisation of a
/// `#[serde(default)]` config calls `Default`, and a command reads config
/// several times per lane, so re-parsing the asset each time dominated the
/// cost of submitting a lane.
fn bundled() -> ReviewConfig {
    static BUNDLED: std::sync::OnceLock<ReviewConfig> = std::sync::OnceLock::new();
    BUNDLED.get_or_init(parse_bundled).clone()
}
#[allow(clippy::panic)]
fn parse_bundled() -> ReviewConfig {
    #[derive(Deserialize)]
    struct Asset {
        review: ReviewAsset,
    }
    #[derive(Deserialize)]
    struct ReviewAsset {
        shapes: BTreeMap<String, ReviewShape>,
        select: SelectAsset,
        size: ReviewSize,
        lenses: LensesAsset,
        #[serde(default)]
        floors: BTreeMap<String, ReviewFloor>,
    }
    #[derive(Deserialize)]
    struct LensesAsset {
        order: Vec<String>,
        by_kind: BTreeMap<String, KindLenses>,
        #[serde(default)]
        by_paths: BTreeMap<String, PathLenses>,
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
        floors: asset.review.floors,
        diversity: FamilyDiversity::default(),
        size: asset.review.size,
        lenses: ReviewLenses {
            order: asset.review.lenses.order,
            by_kind: asset.review.lenses.by_kind,
            by_paths: asset.review.lenses.by_paths,
        },
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
    /// Compile boundary floors from the effective, already layered configuration.
    pub fn workflow_risk_policy(
        &self,
        paths: &[String],
    ) -> provenant_records::workflow::RiskPolicy {
        use provenant_records::workflow::{RiskPolicy, RiskTier};
        let mut policy = RiskPolicy::default();
        for path in paths {
            let floor = self.path_floors(std::slice::from_ref(path));
            let tier = match floor.tier.as_deref() {
                Some("terminal") => RiskTier::Terminal,
                Some("crucial") => RiskTier::Crucial,
                _ => RiskTier::Routine,
            };
            if tier >= RiskTier::Crucial {
                policy.boundary_floors.insert(path.clone(), tier);
            }
        }
        policy
    }
    /// The configured minimum coverage for each changed path. Routine applies
    /// only when no other class matches; unmatched paths use unknown.
    pub fn path_floors(&self, paths: &[String]) -> AppliedFloors {
        fn rank(tier: &str) -> usize {
            match tier {
                "routine" => 0,
                "substantial" => 1,
                "crucial" => 2,
                "terminal" => 3,
                _ => 0,
            }
        }
        let mut result = AppliedFloors::default();
        for path in paths {
            let protected = self.floors.iter().any(|(name, floor)| {
                name != "routine"
                    && name != "unknown"
                    && floor
                        .paths
                        .iter()
                        .any(|pattern| path_matches(pattern, path))
            });
            let mut matched = false;
            for (name, floor) in &self.floors {
                if name == "unknown"
                    || !floor
                        .paths
                        .iter()
                        .any(|pattern| path_matches(pattern, path))
                    || (protected && name == "routine")
                {
                    continue;
                }
                matched = true;
                if !result.classes.contains(name) {
                    result.classes.push(name.clone());
                }
                if rank(&floor.tier) > 0
                    && result
                        .tier
                        .as_deref()
                        .is_none_or(|tier| rank(&floor.tier) > rank(tier))
                {
                    result.tier = Some(floor.tier.clone());
                }
                for lens in &floor.lenses {
                    if !result.lenses.contains(lens) {
                        result.lenses.push(lens.clone());
                    }
                }
            }
            if !matched && let Some(floor) = self.floors.get("unknown") {
                if !result.classes.iter().any(|name| name == "unknown") {
                    result.classes.push("unknown".into());
                }
                if rank(&floor.tier) > 0
                    && result
                        .tier
                        .as_deref()
                        .is_none_or(|tier| rank(&floor.tier) > rank(tier))
                {
                    result.tier = Some(floor.tier.clone());
                }
                for lens in &floor.lenses {
                    if !result.lenses.contains(lens) {
                        result.lenses.push(lens.clone());
                    }
                }
            }
        }
        result
    }

    /// Treat an unreadable path list as potentially matching every floor.
    /// The returned configuration and path are used together for resolution.
    pub fn unreadable_paths(&self) -> (Self, Vec<String>) {
        let mut review = self.clone();
        // The synthetic path represents floors only, not a real path rule.
        review.select.by_paths.clear();
        review.lenses.by_paths.clear();
        let path = "__provenant_unreadable_paths__";
        for special in ["unknown", "routine"] {
            if let Some(floor) = review.floors.remove(special) {
                let mut name = format!("unreadable-{special}");
                while review.floors.contains_key(&name) {
                    name.push('x');
                }
                review.floors.insert(name, floor);
            }
        }
        for floor in review.floors.values_mut() {
            floor.paths.push(path.into());
        }
        (review, vec![path.into()])
    }

    /// The lens order and additions for one change kind.
    pub fn lens_plan(&self, kind: Option<&str>, paths: &[String], test_marker: bool) -> LensPlan {
        let entry = kind.and_then(|kind| self.lenses.by_kind.get(kind));
        let own = entry.filter(|entry| !entry.order.is_empty());
        let matched_paths: Vec<_> = self
            .lenses
            .by_paths
            .iter()
            .filter(|(pattern, _)| paths.iter().any(|path| path_matches(pattern, path)))
            .map(|(pattern, _)| pattern.clone())
            .collect();
        let mut change_add: Vec<String> = Vec::new();
        for pattern in &matched_paths {
            if let Some(rule) = self.lenses.by_paths.get(pattern) {
                change_add.extend(rule.add.iter().cloned());
            }
        }
        if test_marker {
            change_add.push("tests".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        change_add.retain(|lens| seen.insert(lens.clone()));
        let mut add = entry.map(|entry| entry.add.clone()).unwrap_or_default();
        for lens in &change_add {
            if !add.contains(lens) {
                add.push(lens.clone());
            }
        }
        LensPlan {
            order: own.map_or(&self.lenses.order, |entry| &entry.order).clone(),
            exclusive: own.is_some(),
            add,
            change_add,
            matched_paths,
            test_marker,
        }
    }

    // Review requests are ready by definition; preserve configured ready rules.
    fn select_review_rule(
        &self,
        override_value: Option<&String>,
        risk: Option<&str>,
        kind: Option<&str>,
        paths: &[String],
        size: Option<&str>,
    ) -> Selection<String> {
        if override_value.is_none()
            && let Some(value) = self.select.by_readiness.get("ready")
        {
            return Selection {
                value: value.clone(),
                source: "readiness",
                matched: Some("readiness:ready".into()),
            };
        }
        self.select
            .select(override_value, risk, kind, paths, size, None)
    }

    pub fn select_shape<'a>(
        &'a self,
        override_name: Option<&'a str>,
        risk: Option<&str>,
        kind: Option<&str>,
        paths: &[String],
        size: Option<&str>,
    ) -> Result<ReviewSelection<'a>, String> {
        if let Some(name) = override_name
            && !self.shapes.contains_key(name)
        {
            return Err(format!("unknown review shape '{name}'"));
        }
        let floor = self.path_floors(paths);
        let tier_rank = |tier: &str| match tier {
            "routine" => 0,
            "substantial" => 1,
            "crucial" => 2,
            "terminal" => 3,
            _ => 0,
        };
        let risk = match (risk, floor.tier.as_deref()) {
            (Some(requested), Some(minimum)) if tier_rank(minimum) > tier_rank(requested) => {
                Some(minimum)
            }
            (None, Some(minimum)) => Some(minimum),
            (other, _) => other,
        };
        let integration = (kind == Some("integration"))
            .then(|| self.select.by_kind.get("integration"))
            .flatten();
        let mut selected = self.select_review_rule(
            integration.or(override_name.map(str::to_owned).as_ref()),
            risk,
            kind,
            paths,
            size,
        );
        if integration.is_some() {
            selected.source = "by_kind";
        }
        // Risk establishes a floor; size and integration can demand more coverage.
        // A matched path rule is a deliberate choice, so kind and size leave it be.
        {
            let by_paths = selected.source == "by_paths";
            let candidates = [
                (override_name.is_none()
                    && !by_paths
                    && (kind.is_some_and(|k| self.select.by_kind.contains_key(k))
                        || size.is_some_and(|s| self.select.by_size.contains_key(s))))
                .then(|| self.select_review_rule(None, None, kind, &[], size)),
                (override_name.is_none()
                    && !by_paths
                    && kind != Some("docs")
                    && size.is_some_and(|s| self.select.by_size.contains_key(s)))
                .then(|| self.select_review_rule(None, None, None, &[], size)),
                risk.filter(|r| self.select.by_risk.contains_key(*r))
                    .map(|r| self.select_review_rule(None, Some(r), None, &[], None)),
            ];
            for candidate in candidates.into_iter().flatten() {
                let coverage = |name: &str| {
                    self.shapes.get(name).map_or((0, false, 0, false), |shape| {
                        (
                            shape.reviewers.iter().map(|r| r.count).sum::<usize>(),
                            shape.frame,
                            shape.verifiers,
                            shape.synthesis == ReviewSynthesis::Model,
                        )
                    })
                };
                if coverage(&candidate.value) > coverage(&selected.value) {
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

    /// Resolve a review panel without side effects: the shape, then an ordered
    /// slot per selected lens, and the lenses left out with reasons.
    ///
    /// The caller supplies the measured size (an explicit `request.size` wins),
    /// the changed paths and whether the diff has test markers. Every lens the
    /// shape has room for runs. `distinct_families` is carried on the slots as a
    /// preference and never removes one.
    pub fn resolve_panel(
        &self,
        request: &ReviewRequest,
        measured_size: Option<&str>,
        paths: &[String],
        marker: bool,
    ) -> Result<PanelResolution, String> {
        fn text(value: &Option<String>) -> Option<&str> {
            value.as_deref().filter(|text| !text.is_empty())
        }
        if request.minimum_family_counts.len() > 64
            || request
                .minimum_family_counts
                .iter()
                .any(|(family, count)| !identifier(family) || *count > 64)
        {
            return Err("review minimum_family_counts requires at most 64 named families with counts up to 64".into());
        }
        if let Some(lens) = request.lenses.iter().find(|lens| {
            lens.required_family
                .as_ref()
                .is_some_and(|family| !identifier(family))
        }) {
            return Err(format!(
                "required provider family {}: count 1 for lens {} has an invalid family identifier",
                lens.required_family.as_deref().unwrap_or_default(),
                lens.name
            ));
        }
        let size = text(&request.size);
        if let Some(size) = size
            && !["small", "medium", "large"].contains(&size)
        {
            return Err(format!("unknown review size '{size}'"));
        }
        let size = size.or(measured_size);
        let (risk, kind) = (text(&request.risk_tier), text(&request.kind));
        let selected = self.select_shape(text(&request.shape), risk, kind, paths, size)?;

        struct Seat {
            pool: String,
            group_index: usize,
            distinct_families: bool,
        }
        let groups = &selected.shape.reviewers;
        let mut seats = Vec::new();
        let mut named = Vec::new();
        for (group_index, group) in groups.iter().enumerate() {
            let seat = || Seat {
                pool: group.pool.clone(),
                group_index,
                distinct_families: group.distinct_families,
            };
            seats.extend((0..group.count).map(|_| seat()));
            named.extend(group.lenses.iter().map(|lens| (lens.clone(), seat())));
        }
        let limit = seats.len().max(1);

        let mut tags: Vec<&str> = request.risk_tags.iter().map(String::as_str).collect();
        if risk == Some("terminal") {
            tags.push("terminal");
        }
        let mut plan = self.lens_plan(kind, paths, marker);
        let floors = self.path_floors(paths);
        for lens in &floors.lenses {
            if !plan.add.contains(lens) {
                plan.add.push(lens.clone());
            }
            if !plan.change_add.contains(lens) {
                plan.change_add.push(lens.clone());
            }
        }
        let caller_supplied = !request.lenses.is_empty();
        let lens = |name: &str, when: &str| ReviewLens {
            name: name.to_owned(),
            required_family: None,
            when: (!when.is_empty()).then(|| when.to_owned()),
            tier: serde_json::Value::Null,
        };
        let mut list = if caller_supplied {
            request.lenses.clone()
        } else {
            let mut list: Vec<_> = self
                .lenses
                .order
                .iter()
                .map(|name| lens(name, ""))
                .collect();
            for (tag, name) in RISK_TAG_LENSES {
                if !list.iter().any(|item| item.name == name) {
                    list.push(lens(name, tag));
                }
            }
            list
        };
        // A caller's own list keeps its lenses and order; only what the change
        // itself triggers is added to it.
        let additions = if caller_supplied {
            &plan.change_add
        } else {
            &plan.add
        };
        for name in additions {
            if !list.iter().any(|item| &item.name == name) {
                list.push(lens(name, ""));
            }
        }
        if caller_supplied && floors.tier.is_some() {
            for name in &self.lenses.order {
                if !list.iter().any(|item| &item.name == name) {
                    list.push(lens(name, ""));
                }
            }
        }
        if !caller_supplied && !plan.order.is_empty() {
            if plan.exclusive {
                list.retain(|item| {
                    item.when.as_deref().is_some_and(|when| !when.is_empty())
                        || plan.order.contains(&item.name)
                        || plan.add.contains(&item.name)
                });
            }
            list.sort_by_key(|item| {
                plan.order
                    .iter()
                    .position(|name| *name == item.name)
                    .unwrap_or(plan.order.len())
            });
        }

        if !caller_supplied && !named.is_empty() {
            for (name, _) in &named {
                if let Some(item) = list.iter_mut().find(|item| item.name == *name) {
                    item.when = None;
                } else {
                    list.push(lens(name, ""));
                }
            }
            list.sort_by_key(|item| {
                named
                    .iter()
                    .position(|(lens, _)| {
                        item.name == *lens || item.name.starts_with(&format!("{lens}-"))
                    })
                    .unwrap_or(named.len())
            });
        }
        let last = groups.len().saturating_sub(1);
        let mut governed = 0usize;
        let mut slots = Vec::new();
        let mut omitted = Vec::new();
        for item in list {
            let when = item.when.as_deref().unwrap_or_default();
            let triggered = plan.change_add.contains(&item.name);
            if !when.is_empty() && !tags.contains(&when) && !triggered {
                omitted.push(OmittedLens {
                    reason: format!("requires risk tag {when}"),
                    lens: item.name,
                });
                continue;
            }
            if tier_is_set(&item.tier) {
                // An explicit tier overrides the shape: it takes no slot and no
                // family preference, and keeps the group of its pool if any.
                let group_index = item.tier.as_str().map_or(last, |pool| {
                    groups
                        .iter()
                        .position(|group| group.pool == pool)
                        .unwrap_or(last)
                });
                slots.push(ResolvedSlot {
                    lens: item.name,
                    satisfied_family: None,
                    required_family: item.required_family,
                    tier: item.tier,
                    group_index,
                    distinct_families: false,
                });
                continue;
            }
            let mut slot_taken = false;
            if when.is_empty() {
                if governed < limit {
                    governed += 1;
                    slot_taken = true;
                } else if !plan.add.contains(&item.name) {
                    omitted.push(OmittedLens {
                        reason: format!("review shape has {limit} slots"),
                        lens: item.name,
                    });
                    continue;
                }
            }
            let named_seat = named
                .iter()
                .find(|(lens, _)| item.name == *lens || item.name.starts_with(&format!("{lens}-")))
                .map(|(_, seat)| seat);
            let seat =
                named_seat.or_else(|| seats.get(governed.saturating_sub(1)).or(seats.last()));
            let (pool, group_index, distinct_families) = match seat {
                Some(seat) => (
                    seat.pool.as_str(),
                    seat.group_index,
                    // Only lenses that fill a shape slot, or that the shape names,
                    // join a family group.
                    seat.distinct_families && (slot_taken || named_seat.is_some()),
                ),
                None => ("senior", 0, false),
            };
            slots.push(ResolvedSlot {
                lens: item.name,
                satisfied_family: None,
                required_family: item.required_family,
                tier: serde_json::json!(pool),
                group_index,
                distinct_families,
            });
        }
        let mut minimum_family_counts = selected.shape.minimum_family_counts.clone();
        for (family, count) in &request.minimum_family_counts {
            let minimum = minimum_family_counts.entry(family.clone()).or_default();
            *minimum = (*minimum).max(*count);
        }
        Ok(PanelResolution {
            minimum_family_counts,
            shape: selected.name.to_owned(),
            source: selected.source.to_owned(),
            frame: selected.shape.frame,
            verifiers: selected.shape.verifiers,
            synthesis: selected.shape.synthesis,
            slots,
            omitted,
            diversity: request.diversity.unwrap_or(self.diversity),
            warnings: Vec::new(),
        })
    }
    pub(crate) fn validate(
        &self,
        config: &RoutingConfig,
        catalogue: Option<&Catalogue>,
        errors: &mut Vec<SectionError>,
    ) {
        let mut error = |field: String, code| {
            errors.push(SectionError {
                field,
                code,
                detail: None,
            })
        };
        if self.shapes.len() > 128 {
            error("review.shapes".into(), "limit");
        }
        for (name, shape) in &self.shapes {
            let prefix = format!("review.shapes.{name}");
            if !identifier(name) {
                error(prefix.clone(), "selector");
            }
            if shape.minimum_family_counts.len() > 64
                || shape
                    .minimum_family_counts
                    .iter()
                    .any(|(family, count)| !identifier(family) || *count > 64)
            {
                error(format!("{prefix}.minimum_family_counts"), "range");
            }
            if shape.reviewers.is_empty() || shape.reviewers.len() > 32 {
                error(format!("{prefix}.reviewers"), "limit");
            }
            if shape.verifiers > 32 {
                error(format!("{prefix}.verifiers"), "range");
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
        if self.floors.len() > 128 {
            error("review.floors".into(), "limit");
        }
        for (name, floor) in &self.floors {
            let prefix = format!("review.floors.{name}");
            if !identifier(name) {
                error(prefix.clone(), "selector");
            }
            if !["routine", "substantial", "crucial", "terminal"].contains(&floor.tier.as_str()) {
                error(format!("{prefix}.tier"), "selector");
            }
            if floor.paths.len() > 128
                || floor
                    .paths
                    .iter()
                    .any(|path| path.is_empty() || path.len() > 512)
            {
                error(format!("{prefix}.paths"), "selector");
            }
            if floor.lenses.len() > 64 || floor.lenses.iter().any(|lens| !identifier(lens)) {
                error(format!("{prefix}.lenses"), "invalid_lenses");
            }
        }
        let size = &self.size;
        for (field, small, medium) in [
            ("lines", size.small_lines, size.medium_lines),
            ("files", size.small_files, size.medium_files),
            ("crates", size.small_crates, size.medium_crates),
        ] {
            if small > medium {
                error(format!("review.size.small_{field}"), "range");
            }
        }
        if self.lenses.order.is_empty() || self.lenses.order.len() > 64 {
            error("review.lenses.order".into(), "limit");
        }
        for (kind, entry) in &self.lenses.by_kind {
            let prefix = format!("review.lenses.by_kind.{kind}");
            if !identifier(kind) {
                error(prefix.clone(), "selector");
            }
            if entry.order.len() > 64 || entry.add.len() > 64 {
                error(prefix.clone(), "limit");
            }
        }
        for (field, lens) in std::iter::once(("review.lenses.order".to_owned(), &self.lenses.order))
            .chain(self.lenses.by_kind.iter().flat_map(|(kind, entry)| {
                [
                    (format!("review.lenses.by_kind.{kind}.order"), &entry.order),
                    (format!("review.lenses.by_kind.{kind}.add"), &entry.add),
                ]
            }))
            .chain(self.lenses.by_paths.iter().map(|(pattern, entry)| {
                (format!("review.lenses.by_paths.{pattern}.add"), &entry.add)
            }))
        {
            if lens.iter().any(|name| !identifier(name)) {
                error(field, "invalid_lenses");
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
        for (pattern, rule) in &self.lenses.by_paths {
            if pattern.is_empty() || pattern.len() > 512 {
                error(format!("review.lenses.by_paths.{pattern}"), "selector");
            }
            if rule.add.len() > 64 {
                error(format!("review.lenses.by_paths.{pattern}.add"), "limit");
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
/// Shared path exclusions for a small review delta. Callers may add project
/// exclusions; a match is evidence that the path needs a full review.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct ReviewPathExclusions {
    pub patterns: Vec<String>,
}

impl ReviewPathExclusions {
    pub fn bundled() -> Option<Self> {
        toml_edit::de::from_str(include_str!("../assets/review-path-exclusions.toml")).ok()
    }

    pub fn excludes(&self, path: &str) -> bool {
        self.patterns
            .iter()
            .any(|pattern| path_matches(pattern, path))
    }
}

/// Match a repository-relative path against a review policy glob.
pub fn path_matches(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8], memo: &mut [Option<bool>], plen: usize, slen: usize) -> bool {
        let key = (plen - p.len()) * (slen + 1) + slen - s.len();
        if let Some(result) = memo[key] {
            return result;
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
        memo[key] = Some(result);
        result
    }
    // Most policy globs have a literal prefix. Reject unrelated paths before
    // allocating the memo table, and avoid memoising literal-only matches.
    let prefix = pattern.find(['*', '?']).unwrap_or(pattern.len());
    let Some(path) = path.strip_prefix(&pattern[..prefix]) else {
        return false;
    };
    let pattern = &pattern[prefix..];
    if pattern.is_empty() {
        return path.is_empty();
    }
    // Dense offsets avoid a tree lookup and allocation for every visited byte.
    let mut memo = vec![None; (pattern.len() + 1) * (path.len() + 1)];
    go(
        pattern.as_bytes(),
        path.as_bytes(),
        &mut memo,
        pattern.len(),
        path.len(),
    )
}

/// Detect test declarations or common test-runner calls on added or removed
/// diff hunk lines. File headers and hunk metadata are ignored. A marker counts
/// only at the start of the line after indentation, so comments, strings and
/// ordinary method calls such as `object.describe()` do not.
pub fn diff_has_test_markers(diff: &str) -> bool {
    const PREFIXES: [&str; 10] = [
        "#[test]",
        "#[cfg(test)]",
        "#[tokio::test",
        "func Test",
        "def test_",
        "async def test_",
        "@Test",
        "describe",
        "it",
        "test",
    ];
    const CALL_SUFFIXES: [&str; 5] = ["(", ".only(", ".skip(", ".each(", ".concurrent("];
    diff.lines().any(|line| {
        let Some(body) = line
            .strip_prefix('+')
            .filter(|_| !line.starts_with("+++"))
            .or_else(|| line.strip_prefix('-').filter(|_| !line.starts_with("---")))
        else {
            return false;
        };
        let body = body.trim_start();
        PREFIXES.iter().any(|prefix| {
            body.strip_prefix(prefix).is_some_and(|rest| {
                !prefix.chars().all(|c| c.is_ascii_lowercase())
                    || CALL_SUFFIXES.iter().any(|suffix| rest.starts_with(suffix))
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_config::DomainConfig;

    #[test]
    fn workflow_policy_allows_removing_bundled_boundary_floor() {
        use provenant_records::workflow::RiskTier;
        let mut review = ReviewConfig::default();
        let mut custom = review.floors["security"].clone();
        custom.paths = vec!["**/special/**".into()];
        custom.tier = "terminal".into();
        review.floors.insert("custom".into(), custom);
        review.floors.remove("security");
        let paths = vec![
            "src/credentials/store.rs".into(),
            "src/special/a.rs".into(),
            "src/build_graph.rs".into(),
        ];
        let policy = review.workflow_risk_policy(&paths);
        assert!(!policy.boundary_floors.contains_key(&paths[0]));
        assert_eq!(
            policy.boundary_floors.get(&paths[1]),
            Some(&RiskTier::Terminal)
        );
        assert!(!policy.boundary_floors.contains_key(&paths[2]));
    }

    #[test]
    fn size_and_kind_choose_shapes_with_risk_floor() {
        let review = ReviewConfig::default();
        for (lines, files, crates, expected) in [
            (150, 3, 1, "quick"),
            (151, 3, 1, "standard"),
            // Size alone never reaches deep: that is for integration heads and
            // boundary paths.
            (1001, 3, 1, "standard"),
            (1, 16, 1, "standard"),
            (1, 1, 4, "standard"),
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
                .select_shape(None, None, Some("refactor"), &[], Some("small"))
                .unwrap()
                .name,
            "standard"
        );
        assert_eq!(
            review
                .select_shape(None, None, Some("refactor"), &[], Some("large"))
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
        assert_eq!(
            review
                .select_shape(Some("quick"), Some("crucial"), None, &[], None)
                .unwrap()
                .name,
            "deep"
        );
    }

    fn resolved_names(
        review: &ReviewConfig,
        shape: &str,
        kind: Option<&str>,
        paths: &[String],
        marker: bool,
        tags: &[&str],
    ) -> Vec<String> {
        review
            .resolve_panel(
                &ReviewRequest {
                    shape: Some(shape.into()),
                    kind: kind.map(str::to_owned),
                    risk_tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
                    ..Default::default()
                },
                None,
                paths,
                marker,
            )
            .unwrap()
            .slots
            .into_iter()
            .map(|slot| slot.lens)
            .collect()
    }

    #[test]
    fn legacy_panel_selection_matrix() {
        // Capture both selection paths before unifying them. Refactor additions
        // arrive in configured addition order in panel(), but in catalogue order
        // in resolve_panel(); preserve or explicitly decide that difference.
        let mut review = ReviewConfig::default();
        review.floors.clear();
        for (shape, kind, paths, tags, old, resolved) in [
            (
                "quick",
                None,
                vec![],
                vec![],
                vec!["correctness-spec"],
                vec!["correctness-spec"],
            ),
            (
                "standard",
                None,
                vec![],
                vec![],
                vec![
                    "correctness-spec",
                    "tests",
                    "design-fit-reuse",
                    "simplicity",
                    "reliability",
                ],
                vec![
                    "correctness-spec",
                    "tests",
                    "design-fit-reuse",
                    "simplicity",
                    "reliability",
                ],
            ),
            (
                "deep",
                None,
                vec![],
                vec![],
                vec![
                    "correctness-spec",
                    "tests",
                    "design-fit-reuse",
                    "simplicity",
                    "reliability",
                    "conventions-readability",
                    "refactor-cleanup",
                    "flexibility",
                ],
                vec![
                    "correctness-spec",
                    "tests",
                    "design-fit-reuse",
                    "simplicity",
                    "reliability",
                    "conventions-readability",
                    "refactor-cleanup",
                    "flexibility",
                ],
            ),
            (
                "quick",
                Some("docs"),
                vec![],
                vec![],
                vec!["conventions-readability"],
                vec!["conventions-readability"],
            ),
            (
                "quick",
                Some("refactor"),
                vec![],
                vec![],
                vec!["correctness-spec", "refactor-cleanup", "design-fit-reuse"],
                vec!["correctness-spec", "design-fit-reuse", "refactor-cleanup"],
            ),
            (
                "quick",
                None,
                vec!["src/tests/change.rs"],
                vec![],
                vec!["correctness-spec", "tests"],
                vec!["correctness-spec", "tests"],
            ),
            (
                "quick",
                None,
                vec![],
                vec!["security", "concurrency", "terminal"],
                vec![
                    "correctness-spec",
                    "security-privacy",
                    "concurrency-data-errors",
                    "adversarial",
                ],
                vec![
                    "correctness-spec",
                    "security-privacy",
                    "concurrency-data-errors",
                    "adversarial",
                ],
            ),
        ] {
            let paths: Vec<String> = paths.into_iter().map(str::to_owned).collect();
            // The captured old order differs only for quick refactors: the
            // unified selection keeps catalogue order for the added lenses.
            if old != resolved {
                assert_eq!((shape, kind), ("quick", Some("refactor")));
            }
            let request = ReviewRequest {
                shape: Some(shape.into()),
                kind: kind.map(str::to_owned),
                risk_tags: tags.into_iter().map(str::to_owned).collect(),
                ..Default::default()
            };
            let panel = review.resolve_panel(&request, None, &paths, false).unwrap();
            assert_eq!(
                panel
                    .slots
                    .iter()
                    .map(|slot| slot.lens.as_str())
                    .collect::<Vec<_>>(),
                resolved,
                "resolved panel: {shape} {kind:?} {paths:?}"
            );
        }
        let request = ReviewRequest {
            shape: Some("quick".into()),
            lenses: ["reliability", "simplicity"]
                .into_iter()
                .map(|name| ReviewLens {
                    name: name.into(),
                    required_family: None,
                    when: None,
                    tier: serde_json::Value::Null,
                })
                .collect(),
            ..Default::default()
        };
        let panel = review.resolve_panel(&request, None, &[], false).unwrap();
        assert_eq!(
            panel
                .slots
                .iter()
                .map(|slot| slot.lens.as_str())
                .collect::<Vec<_>>(),
            ["reliability"]
        );
        let panel = review
            .resolve_panel(
                &ReviewRequest {
                    risk_tier: Some("crucial".into()),
                    ..request
                },
                None,
                &[],
                false,
            )
            .unwrap();
        assert_eq!(panel.shape, "deep");
    }

    #[test]
    fn protected_floors_raise_shapes_and_extend_caller_lists() {
        let review = ReviewConfig::default();
        let lens = |name: &str| ReviewLens {
            name: name.into(),
            required_family: None,
            when: None,
            tier: serde_json::Value::Null,
        };
        let request = ReviewRequest {
            shape: Some("quick".into()),
            lenses: vec![lens("tests")],
            ..Default::default()
        };
        for (path, class) in [
            ("src/auth/session.rs", "security"),
            ("config/secrets.toml", "security"),
            ("deploy/.env.production", "security"),
            (".github/workflows/release.yml", "external_effects"),
            (".github/workflows/publish-docs.yaml", "external_effects"),
            (
                ".github/workflows/dependabot-automerge.yml",
                "external_effects",
            ),
            (".github/CODEOWNERS", "authority"),
            (
                "tools/front-door/crates/cli/src/run/provider/gh_auth.rs",
                "provenant_credentials",
            ),
            (
                "tools/front-door/crates/run/src/delivery.rs",
                "provenant_credentials",
            ),
            (
                "tools/front-door/crates/confinement/src/utility.rs",
                "provenant_lane_git",
            ),
            ("tools/front-door/src/pr/git.rs", "provenant_lane_git"),
            (
                "tools/front-door/src/pr/landing/gate.rs",
                "provenant_authority",
            ),
            (
                "tools/front-door/crates/confinement/src/worktrees/refs.rs",
                "provenant_lane_git",
            ),
            (
                "tools/front-door/crates/confinement/src/run_config.rs",
                "provenant_lane_git",
            ),
            (
                "tools/front-door/crates/confinement/src/permissions/grants.rs",
                "provenant_lane_git",
            ),
            (
                "tools/front-door/crates/run/src/objects/view.rs",
                "provenant_object_integrity",
            ),
            (
                "tools/front-door/src/pr/landing/publish.rs",
                "provenant_authority",
            ),
            (
                "tools/front-door/crates/fabric-runtime/src/handoff/session.rs",
                "provenant_authority",
            ),
            (
                "tools/front-door/src/bin/package-release.rs",
                "provenant_release",
            ),
            ("src/config/parser.rs", "contract"),
            ("src/cli/run.rs", "contract"),
        ] {
            assert!(
                review
                    .path_floors(&[path.into()])
                    .classes
                    .contains(&class.to_owned()),
                "{path}"
            );
        }
        // Paths that matched only a removed substring are no longer crucial.
        for path in [
            "src/object_receive.rs",
            "src/build_graph.rs",
            "src/authority_notes.rs",
            "src/author_list.rs",
            "src/chair_view.rs",
            "src/git/refs.rs",
            "src/security_banner.rs",
            "src/release_notes.rs",
            "Cargo.toml",
            ".github/workflows/ci.yml",
        ] {
            let floors = review.path_floors(&[path.into()]);
            assert_ne!(floors.tier.as_deref(), Some("crucial"), "{path}");
            let panel = review
                .resolve_panel(&request, None, &[path.into()], false)
                .unwrap();
            assert_ne!(panel.shape, "deep", "{path}");
        }
        let paths = vec!["src/credentials/store.rs".into()];
        let panel = review.resolve_panel(&request, None, &paths, false).unwrap();
        assert_eq!(panel.shape, "deep");
        let names: Vec<_> = panel.slots.iter().map(|slot| slot.lens.as_str()).collect();
        assert!(names.contains(&"tests"));
        assert!(names.contains(&"security-privacy"));
        assert!(names.contains(&"correctness-spec"));
        assert!(names.len() >= 8);
        assert!(
            review
                .path_floors(&paths)
                .classes
                .contains(&"security".to_owned())
        );
        let unknown = review
            .resolve_panel(&request, None, &["src/widget.rs".into()], false)
            .unwrap();
        assert_eq!(unknown.shape, "standard");
        let docs = review
            .resolve_panel(&request, None, &["docs/guide.md".into()], false)
            .unwrap();
        assert_eq!(docs.shape, "quick");
        assert_eq!(docs.slots.len(), 1);
        let mut equal_count = review.clone();
        equal_count.shapes.get_mut("standard").unwrap().reviewers[0].count = 1;
        let panel = equal_count
            .resolve_panel(&request, None, &["src/widget.rs".into()], false)
            .unwrap();
        assert_eq!(panel.shape, "standard");
        assert!(panel.frame && panel.verifiers > 0);
    }

    #[test]
    fn unreadable_paths_include_every_configured_floor() {
        let mut review = ReviewConfig::default();
        let routine = review.floors.get_mut("routine").unwrap();
        routine.tier = "terminal".into();
        routine.lenses.push("adversarial".into());
        let (fallback, paths) = review.unreadable_paths();
        let floors = fallback.path_floors(&paths);
        assert_eq!(floors.tier.as_deref(), Some("terminal"));
        assert!(floors.lenses.contains(&"adversarial".into()));
        assert!(floors.lenses.contains(&"security-privacy".into()));
    }

    #[test]
    fn overlapping_markdown_applies_bundled_and_project_floors() {
        let mut review = ReviewConfig::default();
        review.floors.insert(
            "project".into(),
            ReviewFloor {
                paths: vec!["docs/private.md".into()],
                tier: "terminal".into(),
                lenses: vec!["adversarial".into()],
            },
        );
        let request = ReviewRequest {
            shape: Some("quick".into()),
            ..Default::default()
        };
        for (path, class, required) in [
            ("docs/credentials/guide.md", "security", "security-privacy"),
            ("docs/private.md", "project", "adversarial"),
        ] {
            let paths = vec![path.to_owned()];
            let floors = review.path_floors(&paths);
            assert!(floors.classes.contains(&class.to_owned()));
            assert!(!floors.classes.contains(&"routine".to_owned()));
            let panel = review.resolve_panel(&request, None, &paths, false).unwrap();
            assert_ne!(panel.shape, "quick");
            assert!(panel.slots.iter().any(|slot| slot.lens == required));
        }
    }

    #[test]
    fn selected_panels_keep_change_driven_lenses() {
        let mut review = ReviewConfig::default();
        review.floors.clear();
        assert_eq!(
            resolved_names(&review, "standard", None, &[], false, &[]),
            [
                "correctness-spec",
                "tests",
                "design-fit-reuse",
                "simplicity",
                "reliability"
            ]
        );
        assert_eq!(
            resolved_names(&review, "quick", Some("refactor"), &[], false, &[]),
            ["correctness-spec", "design-fit-reuse", "refactor-cleanup"]
        );
        assert_eq!(
            resolved_names(
                &review,
                "quick",
                None,
                &["crates/x/tests/a.rs".into()],
                false,
                &[]
            ),
            ["correctness-spec", "tests"]
        );
        assert_eq!(
            resolved_names(&review, "quick", None, &["src/lib.rs".into()], true, &[]),
            ["correctness-spec", "tests"]
        );
        assert_eq!(
            resolved_names(&review, "quick", None, &["src/lib.rs".into()], false, &[]),
            ["correctness-spec"]
        );
    }

    #[test]
    fn test_marker_scan_only_reads_added_or_removed_hunk_lines() {
        for (diff, expected) in [
            ("@@ -1 +1 @@\n-#[cfg(test)]\n+#[tokio::test]\n", true),
            (
                "@@ -1 +1 @@\n+#[tokio::test(flavor = \"multi_thread\")]\n",
                true,
            ),
            ("@@ -1,0 +2 @@\n+describe(\"suite\", () => {})\n", true),
            ("@@ -1 +1 @@\n+    it(\"works\", () => {});\n", true),
            ("@@ -1 +1 @@\n-test(\"works\", () => {});\n", true),
            ("@@ -1 +1 @@\n+  it.each([1])(\"n\", () => {});\n", true),
            ("@@ -1 +1 @@\n+func TestParse(t *testing.T) {\n", true),
            ("@@ -1 +1 @@\n+def test_parse():\n", true),
            ("@@ -1 +1 @@\n+    @Test func parses() {}\n", true),
            // Context lines and file headers are not changes.
            ("@@ -1 +1 @@\n context #[test]\n", false),
            ("+++ b/src/lib.rs\n--- a/src/lib.rs\n", false),
            // Ordinary code, comments and strings are not test declarations.
            ("@@ -1 +1 @@\n+let result = source.split(',');\n", false),
            ("@@ -1 +1 @@\n+return object.describe();\n", false),
            (
                "@@ -1 +1 @@\n+    let text = widget.it(x) + test(y);\n",
                false,
            ),
            ("@@ -1 +1 @@\n+    submit(order);\n", false),
            ("@@ -1 +1 @@\n+    items(order);\n", false),
            (
                "@@ -1 +1 @@\n+// #[test] describe(\"x\") it(\"y\")\n",
                false,
            ),
            ("@@ -1 +1 @@\n+/// #[tokio::test]\n", false),
            ("@@ -1 +1 @@\n+# def test_parse():\n", false),
            ("@@ -1 +1 @@\n+ * describe(\"x\")\n", false),
            ("@@ -1 +1 @@\n+let marker = \"#[test]\";\n", false),
            (
                "@@ -1 +1 @@\n+let marker = \"describe(\\\"x\\\")\";\n",
                false,
            ),
            ("@@ -1 +1 @@\n+    \"it(x)\",\n", false),
        ] {
            assert_eq!(diff_has_test_markers(diff), expected, "{diff:?}");
        }
    }

    #[test]
    fn docs_panels_use_one_base_lens_and_only_risk_extras() {
        let review = ReviewConfig::default();
        assert_eq!(
            resolved_names(&review, "deep", Some("docs"), &[], false, &[]),
            ["conventions-readability"]
        );
        assert_eq!(
            resolved_names(&review, "deep", Some("docs"), &[], false, &["security"]),
            ["conventions-readability", "security-privacy"]
        );
    }

    #[test]
    fn risk_lenses_follow_the_shared_table() {
        assert_eq!(risk_lenses(&["security"], None), ["security-privacy"]);
        assert_eq!(risk_lenses::<&str>(&[], Some("terminal")), ["adversarial"]);
        assert_eq!(
            risk_lenses(&["concurrency", "security"], None),
            ["security-privacy", "concurrency-data-errors"]
        );
        assert!(risk_lenses::<&str>(&[], Some("crucial")).is_empty());
        assert_eq!(risk_tag_names(), ["security", "concurrency", "terminal"]);
    }

    #[test]
    fn explicit_shape_lenses_survive_and_are_not_repeated() {
        let mut review = ReviewConfig::default();
        let shape = ReviewShape {
            minimum_family_counts: BTreeMap::new(),
            reviewers: vec![
                ReviewReviewer {
                    pool: "junior".into(),
                    count: 1,
                    lenses: vec!["tests".into()],
                    distinct_families: false,
                },
                ReviewReviewer {
                    pool: "senior".into(),
                    count: 2,
                    lenses: vec![],
                    distinct_families: false,
                },
            ],
            frame: true,
            verifiers: 1,
            synthesis: ReviewSynthesis::Model,
        };
        review.shapes.insert("custom".into(), shape);
        assert_eq!(
            resolved_names(&review, "custom", None, &[], false, &[]),
            ["tests", "correctness-spec", "design-fit-reuse"]
        );
    }

    #[test]
    fn path_rules_are_not_overridden_by_size_or_kind() {
        let mut review = ReviewConfig::default();
        review
            .select
            .by_paths
            .insert("docs/**".into(), "quick".into());
        let paths = vec!["docs/a.md".to_owned()];
        let selected = review
            .select_shape(None, None, Some("refactor"), &paths, Some("large"))
            .unwrap();
        assert_eq!((selected.name, selected.source), ("quick", "by_paths"));
        // The risk floor still applies over a path rule.
        let selected = review
            .select_shape(None, Some("crucial"), None, &paths, Some("large"))
            .unwrap();
        assert_eq!(selected.name, "deep");
    }

    #[test]
    fn measure_degrades_to_unknown_and_skips_integration() {
        let size = ReviewSize::default();
        let mut fail = |_: &[&str]| -> Result<String, String> { Err("fatal: bad object".into()) };
        let measured = size.measure(None, "main", "HEAD", &mut fail);
        assert_eq!(measured.size, None);
        assert!(measured.warning.unwrap().contains("fatal: bad object"));
        let mut never = |_: &[&str]| -> Result<String, String> { panic!("must not run git") };
        assert_eq!(
            size.measure(Some("integration"), "main", "HEAD", &mut never),
            Measured::default()
        );
        assert!(
            size.measure(None, "-x", "HEAD", &mut never)
                .warning
                .is_some()
        );
    }

    #[test]
    fn measure_classifies_the_merge_base_diff() {
        let size = ReviewSize::default();
        let mut git = |args: &[&str]| -> Result<String, String> {
            Ok(match args[0] {
                "merge-base" => "abc\n".into(),
                "diff" => {
                    assert!(args.contains(&"abc"));
                    "400\t0\ta.rs\0".into()
                }
                _ => String::new(),
            })
        };
        assert_eq!(
            size.measure(None, "main", "HEAD", &mut git),
            Measured {
                size: Some("medium"),
                warning: None
            }
        );
        let mut empty = |args: &[&str]| -> Result<String, String> {
            Ok(if args[0] == "merge-base" {
                "abc".into()
            } else {
                String::new()
            })
        };
        assert_eq!(
            size.measure(None, "main", "HEAD", &mut empty),
            Measured::default()
        );
    }

    #[test]
    fn size_thresholds_must_be_ordered() {
        let mut config: RoutingConfig =
            toml_edit::de::from_str(include_str!("../assets/review-defaults.toml")).unwrap();
        assert!(config.validate().is_empty());
        config.review.size.small_files = 99;
        assert!(
            config
                .validate()
                .iter()
                .any(|e| e.field == "review.size.small_files" && e.code == "range")
        );
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
                "20\t3\tapps/a/src/main.rs\x001\t1\tapps/b/src/lib.rs\0",
                trees
            ),
            "medium"
        );
        assert_eq!(size.classify_numstat("-\t-\timage.png\0", trees), "large");
    }

    #[test]
    fn bundled_review_shapes_cover_all_risk_tiers() {
        let review = ReviewConfig::default();
        for tier in ["routine", "substantial", "crucial", "terminal"] {
            let selected = review
                .select_shape(None, Some(tier), None, &[], None)
                .unwrap();
            assert!(
                review.shapes.contains_key(selected.name),
                "risk tier {tier}"
            );
            assert_eq!(selected.source, "by_risk", "risk tier {tier}");
        }
    }

    #[test]
    fn review_path_globs_preserve_literal_and_component_boundaries() {
        for (pattern, path, expected) in [
            ("", "", true),
            ("", "src", false),
            ("docs/run.md", "docs/run.md", true),
            ("docs/run.md", "docs/run.md.bak", false),
            ("docs/run.md", "docs/run", false),
            ("src/*.rs", "docs/lib.rs", false),
            ("src/*.rs", "src/lib.rs", true),
            ("src/*.rs", "src/nested/lib.rs", false),
            ("src/**/test?.rs", "src/test1.rs", true),
            ("src/**/test?.rs", "src/a/b/test1.rs", true),
            ("src/**/test?.rs", "src/test12.rs", false),
            ("src/?/lib.rs", "src/a/lib.rs", true),
            ("src/?/lib.rs", "src/a/b/lib.rs", false),
            ("**/*.md", "README.md", true),
            ("**/*.md", "docs/specs/run.md", true),
            ("docs/é*.md", "docs/été.md", true),
            ("**", "", true),
            ("**", "src/a/b.rs", true),
        ] {
            assert_eq!(path_matches(pattern, path), expected, "{pattern}: {path}");
        }
    }

    #[test]
    fn selection_precedence_and_path_globs() {
        let mut review = ReviewConfig::default();
        review.floors.clear();
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
    fn review_shape_honours_ready_rule() {
        let mut review = ReviewConfig::default();
        let name = review.select.default.clone();
        review
            .select
            .by_readiness
            .insert("ready".into(), name.clone());
        let selected = review.select_shape(None, None, None, &[], None).unwrap();
        assert_eq!(selected.name, name);
        assert_eq!(selected.source, "readiness");
    }

    #[test]
    fn generic_select_supports_size_and_readiness_rules() {
        let select = Select {
            default: "single".to_owned(),
            by_risk: BTreeMap::from([("critical".into(), "review".into())]),
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
            "review"
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
        let quick = &config.review.shapes["quick"];
        assert!(!quick.frame);
        assert_eq!(quick.verifiers, 0);
        assert_eq!(quick.synthesis, ReviewSynthesis::Reduce);
        let standard = &config.review.shapes["standard"];
        assert!(standard.frame);
        assert_eq!(standard.verifiers, 1);
        assert_eq!(standard.synthesis, ReviewSynthesis::Model);
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

    fn request_lens(name: &str, tier: serde_json::Value) -> ReviewLens {
        ReviewLens {
            name: name.into(),
            required_family: None,
            when: None,
            tier,
        }
    }

    fn resolve(request: &ReviewRequest) -> PanelResolution {
        ReviewConfig::default()
            .resolve_panel(request, None, &[], false)
            .unwrap()
    }

    #[test]
    fn panel_resolution_applies_risk_floor_kind_and_integration() {
        let review = ReviewConfig::default();
        let floor = ReviewRequest {
            risk_tier: Some("crucial".into()),
            size: Some("small".into()),
            ..Default::default()
        };
        let floored = review
            .resolve_panel(&floor, Some("small"), &[], false)
            .unwrap();
        assert_eq!(
            (floored.shape.as_str(), floored.source.as_str()),
            ("deep", "by_risk")
        );

        let integration = ReviewRequest {
            kind: Some("integration".into()),
            size: Some("small".into()),
            ..Default::default()
        };
        let integration = review
            .resolve_panel(&integration, Some("small"), &[], false)
            .unwrap();
        assert_eq!(
            (integration.shape.as_str(), integration.source.as_str()),
            ("deep", "by_kind")
        );

        let docs = resolve(&ReviewRequest {
            kind: Some("docs".into()),
            ..Default::default()
        });
        assert!(!docs.frame);
        assert_eq!(docs.verifiers, 0);
        assert_eq!(docs.synthesis, ReviewSynthesis::Reduce);
        assert_eq!(docs.slots.len(), 1);
        assert_eq!(docs.slots[0].lens, "conventions-readability");
        assert!(
            docs.omitted
                .iter()
                .any(|o| o.reason.starts_with("requires risk tag"))
        );

        let refactor = resolve(&ReviewRequest {
            kind: Some("refactor".into()),
            ..Default::default()
        });
        assert!(refactor.slots.iter().any(|s| s.lens == "refactor-cleanup"));

        let standard = resolve(&ReviewRequest::default());
        assert!(standard.frame);
        assert_eq!(standard.verifiers, 1);
        assert_eq!(standard.synthesis, ReviewSynthesis::Model);
        assert!(
            standard
                .omitted
                .iter()
                .any(|o| o.reason == "review shape has 5 slots")
        );

        let unknown = ReviewRequest {
            size: Some("huge".into()),
            ..Default::default()
        };
        assert!(review.resolve_panel(&unknown, None, &[], false).is_err());
    }

    #[test]
    fn panel_resolution_retains_route_table_and_exact_model_tiers() {
        let table = serde_json::json!({"model": "claude-sonnet", "effort": "high"});
        let list = serde_json::json!([{"alias": "junior"}, {"alias": "senior"}]);
        let exact = serde_json::json!("anthropic/claude-sonnet@high");
        let resolution = resolve(&ReviewRequest {
            shape: Some("quick".into()),
            lenses: vec![
                request_lens("correctness-spec", table.clone()),
                request_lens("tests", exact.clone()),
                request_lens("simplicity", list.clone()),
                request_lens("design-fit-reuse", serde_json::Value::Null),
            ],
            ..Default::default()
        });
        let tiers: Vec<_> = resolution.slots.iter().map(|s| &s.tier).collect();
        assert_eq!(tiers[..3], [&table, &exact, &list]);
        // Explicit tiers take no shape slot, so the untiered lens takes the one slot.
        assert_eq!(tiers[3], &serde_json::json!("junior"));
        assert!(resolution.omitted.is_empty());
    }

    #[test]
    fn panel_resolution_carries_group_flags() {
        let quick = resolve(&ReviewRequest {
            shape: Some("quick".into()),
            ..Default::default()
        });
        assert_eq!(quick.slots.len(), 1);
        assert!(!quick.slots[0].distinct_families);

        let standard = resolve(&ReviewRequest {
            shape: Some("standard".into()),
            ..Default::default()
        });
        assert_eq!(standard.slots.len(), 5);
        for slot in &standard.slots {
            assert_eq!(slot.group_index, 0);
            assert!(slot.distinct_families);
            assert_eq!(slot.tier, serde_json::json!("junior"));
        }

        let deep = resolve(&ReviewRequest {
            shape: Some("deep".into()),
            ..Default::default()
        });
        let placed: Vec<_> = deep
            .slots
            .iter()
            .map(|s| (s.group_index, s.tier.as_str().unwrap()))
            .collect();
        assert_eq!(placed[..4], [(0, "junior"); 4]);
        assert_eq!(placed[4..8], [(1, "senior"); 4]);
        assert!(deep.slots[..8].iter().all(|s| s.distinct_families));
    }

    #[test]
    fn distinct_families_is_a_preference_that_never_drops_a_lens() {
        // The resolver knows no catalogue, so it cannot tell that the pool has a
        // single family. Whatever the pool holds, every lens the shape has room
        // for stays in the panel and the flag only records the preference.
        let panel = resolve(&ReviewRequest {
            shape: Some("standard".into()),
            lenses: ["correctness-spec", "tests", "simplicity"]
                .map(|name| request_lens(name, serde_json::Value::Null))
                .to_vec(),
            ..Default::default()
        });
        let lenses: Vec<_> = panel.slots.iter().map(|s| s.lens.as_str()).collect();
        assert_eq!(lenses, ["correctness-spec", "tests", "simplicity"]);
        assert!(panel.slots.iter().all(|s| s.distinct_families));
        assert!(panel.omitted.is_empty());
    }
    #[test]
    fn family_floors_keep_shape_minimum_and_record_matching_legs() {
        let mut review = ReviewConfig::default();
        review
            .shapes
            .get_mut("standard")
            .unwrap()
            .minimum_family_counts = BTreeMap::from([("alpha".into(), 2)]);
        let request = ReviewRequest {
            shape: Some("standard".into()),
            minimum_family_counts: BTreeMap::from([("alpha".into(), 1), ("beta".into(), 1)]),
            lenses: ["correctness", "tests", "design"]
                .map(|name| request_lens(name, serde_json::Value::Null))
                .to_vec(),
            ..Default::default()
        };
        let mut panel = review.resolve_panel(&request, None, &[], false).unwrap();
        assert_eq!(panel.minimum_family_counts["alpha"], 2);
        // Correctness can serve either family; the other lenses can only serve
        // alpha. A greedy first assignment would leave beta unsatisfied.
        panel
            .resolve_families(|slot, family| {
                let family = family.unwrap();
                if slot.lens == "correctness" || family == "alpha" {
                    Ok(family.into())
                } else {
                    Err("pool has no member of this family".into())
                }
            })
            .unwrap();
        assert_eq!(panel.slots[0].satisfied_family.as_deref(), Some("beta"));
        assert_eq!(panel.slots[1].satisfied_family.as_deref(), Some("alpha"));
        assert_eq!(panel.slots[2].satisfied_family.as_deref(), Some("alpha"));
        let recorded = serde_json::to_value(&panel).unwrap();
        assert_eq!(recorded["slots"][0]["satisfied_family"], "beta");
    }

    #[test]
    fn required_lens_family_counts_towards_panel_minimum_and_refuses_with_reason() {
        let mut lens = request_lens("tests", serde_json::json!("senior"));
        lens.required_family = Some("alpha".into());
        let mut panel = resolve(&ReviewRequest {
            shape: Some("quick".into()),
            minimum_family_counts: BTreeMap::from([("alpha".into(), 1)]),
            lenses: vec![lens],
            ..Default::default()
        });
        let mut probes = 0;
        panel
            .resolve_families(|slot, family| {
                probes += 1;
                assert_eq!(slot.lens, "tests");
                assert_eq!(family, Some("alpha"));
                Ok("alpha".into())
            })
            .unwrap();
        assert_eq!(probes, 1);
        assert_eq!(panel.slots[0].required_family.as_deref(), Some("alpha"));
        assert_eq!(panel.slots[0].satisfied_family.as_deref(), Some("alpha"));
        let error = panel
            .resolve_families(|_, _| Err("account excluded by policy".into()))
            .unwrap_err();
        assert!(error.contains("family alpha"));
        assert!(error.contains("count 1"));
        assert!(error.contains("tests"));
        assert!(error.contains("account excluded by policy"));
    }

    #[test]
    fn unsatisfied_panel_family_warns_by_default_and_refuses_when_strict() {
        let mut request = ReviewRequest {
            minimum_family_counts: BTreeMap::from([("alpha".into(), 2)]),
            ..Default::default()
        };
        let mut panel = resolve(&request);
        panel
            .resolve_families(|slot, _| Err(format!("{} account unavailable", slot.lens)))
            .unwrap();
        assert_eq!(panel.warnings.len(), 1);
        assert!(panel.warnings[0].contains("family alpha"));
        assert!(panel.warnings[0].contains("count 2"));
        assert!(panel.warnings[0].contains("account unavailable"));
        assert!(
            panel
                .slots
                .iter()
                .all(|slot| slot.satisfied_family.is_none())
        );
        request.diversity = Some(FamilyDiversity::Strict);
        let mut panel = resolve(&request);
        let error = panel
            .resolve_families(|slot, _| Err(format!("{} account unavailable", slot.lens)))
            .unwrap_err();
        assert!(error.contains("family alpha"));
        assert!(error.contains("count 2"));
        assert!(error.contains("account unavailable"));
    }

    #[test]
    fn unconstrained_panel_never_probes_or_changes_routes() {
        let mut panel = resolve(&ReviewRequest::default());
        let before = serde_json::to_value(&panel).unwrap();
        panel
            .resolve_families(|_, _| -> Result<String, String> {
                panic!("unconstrained reviews must retain ordinary route selection")
            })
            .unwrap();
        assert_eq!(serde_json::to_value(&panel).unwrap(), before);
        assert!(before.get("minimum_family_counts").is_none());
        assert!(before["slots"][0].get("required_family").is_none());
        assert!(before["slots"][0].get("satisfied_family").is_none());
    }
}
