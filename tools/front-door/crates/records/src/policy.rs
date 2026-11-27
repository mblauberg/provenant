//! One executable policy table, shared by enforcement and presentation.
use crate::command_result::SettingRef;
use crate::provenance::{CANONICAL_ORDER, Layer, Source as ProvenanceSource, Sourced};
use crate::warning::{
    self, CHECK_NOT_OVERRIDABLE, CONFIG_KEY_IGNORED, CONFIG_MALFORMED, Warning, WarningCode,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::OnceLock};

pub const WRITER_HANDOVER: &str = "WRITER_HANDOVER";
pub const WRITER_HEAD_MOVED: &str = "WRITER_HEAD_MOVED";
pub const WORKTREE_SHARED: &str = "WORKTREE_SHARED";
pub const WORKTREE_SHARING_REFUSED: &str = "WORKTREE_SHARING_REFUSED";
pub const PATH_CLAIM_OVERLAP: &str = "PATH_CLAIM_OVERLAP";
pub const DESTROYS_LIVE_WORK: &str = "DESTROYS_LIVE_WORK";
pub const LEASE_HOLDER_UNREACHABLE: &str = "LEASE_HOLDER_UNREACHABLE";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Boundary {
    /// Credentials and secrets never reach a lane, provider or output not granted them.
    Credentials,
    /// No lane moves, rewrites or deletes another lane's refs, commits or uncommitted files.
    OtherWork,
    /// User gates, merge, release and ruleset changes need the held authority.
    Authority,
    /// Not a boundary: a typed operation error (malformed input, missing capability,
    /// invalid state, failed integrity check). It refuses that one operation and has
    /// no override because proceeding cannot work.
    Operation,
    /// Not a boundary: a mechanism or default that warns and proceeds.
    #[default]
    None,
}

impl Boundary {
    /// Whether a violated rule refuses without any override: the three
    /// boundaries and operation errors.
    pub const fn is_immutable(self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strength {
    Refuse,
    Warn,
    Off,
}

impl Strength {
    pub const ALL: [Self; 3] = [Self::Refuse, Self::Warn, Self::Off];

    pub const fn behaviour(self) -> &'static str {
        match self {
            Self::Refuse => "block the violated predicate",
            Self::Warn => "warn and proceed",
            Self::Off => "proceed without a policy warning",
        }
    }
}

/// What a launch's `--strict` and `--checks` flags select for `policy.default`.
/// `On` leaves the layered configuration alone; the other two modes insert a
/// launch-layer default, so they can never raise a boundary or operation row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksMode {
    #[default]
    On,
    Warn,
    Strict,
    Off,
}

impl ChecksMode {
    /// Parse the launch `--checks` values. `strict` is accepted as the
    /// spelling of `--strict` for callers that pass one combined value.
    pub fn from_checks(value: &str) -> Option<Self> {
        match value {
            "on" => Some(Self::On),
            "warn" => Some(Self::Warn),
            "off" => Some(Self::Off),
            "refuse" | "strict" => Some(Self::Strict),
            _ => None,
        }
    }
}

/// The provenance of one policy candidate. A dispatch override arrives with
/// the resolved route's policy; a `--strict` / `--checks` default arrives at
/// the launch layer, which outranks every host and project `policy.<CODE>`.
fn layer_source(layer: Layer, key: &str) -> ProvenanceSource {
    let origin = match layer {
        Layer::Default => "compiled policy table".to_owned(),
        Layer::Host => format!("config.toml#policy.{key}"),
        Layer::Project => format!(".provenant/policy.toml#policy.{key}"),
        Layer::Route => format!("dispatch#policy.{key}"),
        Layer::Launch => format!("--checks/--strict#policy.{key}"),
        other => format!("{}#policy.{key}", other.as_str()),
    };
    ProvenanceSource {
        layer,
        origin: Some(origin),
    }
}

/// The setting that selected an effect, for the error and warning contract.
fn setting_ref(key: &str, strength: Strength, source: &ProvenanceSource) -> SettingRef {
    SettingRef {
        key: key.to_owned(),
        value: strength_name(strength).to_owned(),
        layer: source.layer,
        origin: source
            .origin
            .clone()
            .unwrap_or_else(|| "compiled policy table".to_owned()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub code: String,
    pub boundary: Boundary,
    pub strength: Strength,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub knob: Option<String>,
    pub justification: String,
    pub site: String,
    pub threshold: Option<String>,
    /// A non-boundary rule may offer a strict, user-selected stop only
    /// when its consumer has a meaningful stop path.
    #[serde(default)]
    pub refusable: bool,
}

/// Policy setting candidates grouped by their fully qualified key, for
/// example `policy.CODE` or `policy.default`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub candidates: BTreeMap<String, Vec<Sourced<Strength>>>,
}

impl Settings {
    /// Add a candidate while preserving caller order within a layer.
    pub fn insert(&mut self, key: impl Into<String>, value: Sourced<Strength>) {
        self.candidates.entry(key.into()).or_default().push(value);
    }
}

/// Resolve a rule's setting by layer first, then key specificity, and report
/// the winning key so a warning can name the exact candidate that selected it.
/// `policy.CODE` wins ties against `policy.GROUP`, which wins ties against
/// `policy.default`; a higher layer wins across keys. A row's compiled
/// strength is the final fallback.
#[allow(clippy::expect_used)] // callers resolve codes from the compiled policy table
pub fn resolve_setting(
    code: &str,
    group: Option<&str>,
    settings: &Settings,
) -> (String, Sourced<Strength>) {
    let mut keys = vec![(format!("policy.{code}"), 3usize)];
    if let Some(group) = group {
        keys.push((format!("policy.{group}"), 2));
    }
    keys.push(("policy.default".to_owned(), 1));

    let mut winner: Option<(usize, usize, String, Sourced<Strength>)> = None;
    for (key, specificity) in keys {
        let Some(candidates) = settings.candidates.get(&key) else {
            continue;
        };
        for candidate in candidates {
            let layer_rank = CANONICAL_ORDER
                .iter()
                .position(|layer| *layer == candidate.source.layer)
                .expect("candidate layer is canonical");
            let should_replace =
                winner
                    .as_ref()
                    .is_none_or(|(best_layer, best_specificity, _, _)| {
                        layer_rank > *best_layer
                            || (layer_rank == *best_layer && specificity >= *best_specificity)
                    });
            if should_replace {
                winner = Some((layer_rank, specificity, key.clone(), candidate.clone()));
            }
        }
    }
    if let Some((_, _, key, winner)) = winner {
        return (key, winner);
    }

    let rule = table()
        .iter()
        .find(|rule| rule.code == code)
        .expect("policy code in table");
    (
        format!("policy.{code}"),
        Sourced {
            value: rule.strength,
            source: layer_source(Layer::Default, &rule.code),
        },
    )
}

/// Resolve a rule's effective strength. See [`resolve_setting`] when the
/// winning key and layer are needed for a setting reference.
pub fn resolve_strength(code: &str, group: Option<&str>, settings: &Settings) -> Sourced<Strength> {
    resolve_setting(code, group, settings).1
}
impl Rule {
    pub fn supports(&self, strength: Strength) -> bool {
        match (self.boundary, strength) {
            (Boundary::None, Strength::Refuse) => self.refusable,
            (Boundary::None, Strength::Warn | Strength::Off) => true,
            (
                Boundary::Credentials
                | Boundary::OtherWork
                | Boundary::Authority
                | Boundary::Operation,
                Strength::Refuse,
            ) => true,
            _ => false,
        }
    }

    /// Generated guidance follows the same supported strengths as enforcement.
    pub fn behaviour(&self) -> String {
        let strengths = self
            .strengths()
            .into_iter()
            .map(|strength| format!("{}: {}", strength_name(strength), strength.behaviour()))
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "Policy {} (default={}, source=compiled default; override={}): {}. For a rule with boundary none, refuse means a configured stop; boundary and operation refusals are immutable.",
            self.code,
            strength_name(self.strength),
            self.knob.as_deref().unwrap_or("none; immutable"),
            strengths
        )
    }

    pub fn strengths(&self) -> Vec<Strength> {
        Strength::ALL
            .into_iter()
            .filter(|strength| self.supports(*strength))
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveRule {
    #[serde(flatten)]
    pub rule: Rule,
    /// The provenance layer and origin that last set this row's strength.
    pub source: ProvenanceSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub rules: Vec<EffectiveRule>,
    /// All policy setting candidates, keyed by their fully qualified key, so
    /// a `none` row resolves from `policy.<CODE>`, `policy.<GROUP>` and
    /// `policy.default` with the layer rule shared by every setting family.
    #[serde(default)]
    pub settings: Settings,
}

/// A heuristic finding identifies its field and rule without retaining matched values.
/// Scanners report findings; they do not select policy effects or stop execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScannerFinding {
    pub field: String,
    pub rule: String,
}

/// A caller supplies a failed predicate and its observed facts, never a decision.
pub struct Facts {
    pub violated: bool,
    pub detail: String,
}
impl Facts {
    pub fn violation(detail: impl Into<String>) -> Self {
        Self {
            violated: true,
            detail: detail.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)] // Warn is the common non-proceed effect; boxing it costs every caller.
pub enum Effect {
    Proceed,
    Warn(Warning),
    Refuse(Box<crate::diagnostic::Diagnostic>),
    Stop(Box<crate::diagnostic::Diagnostic>),
}
impl Effect {
    pub fn blocks(&self) -> bool {
        matches!(self, Self::Refuse(_) | Self::Stop(_))
    }
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Proceed => None,
            Self::Warn(warning) => Some(&warning.message),
            Self::Refuse(diag) | Self::Stop(diag) => diag.detail.as_deref(),
        }
    }
}

#[derive(Deserialize)]
struct Table {
    rule: Vec<Rule>,
}
/// Parsed once per process. Resolution copies only this small immutable table.
#[allow(clippy::expect_used)] // Compiled asset; invariant validated in CI.
pub fn table() -> &'static [Rule] {
    static TABLE: OnceLock<Vec<Rule>> = OnceLock::new();
    TABLE.get_or_init(|| {
        toml_edit::de::from_str::<Table>(include_str!("../../run/assets/policy.toml"))
            .expect("compiled policy table")
            .rule
    })
}

/// A violated policy rule warns under its own stable rule code, so each code
/// names one event instead of sharing a catch-all `POLICY_WARNING`. The code is
/// read from the compiled table, the authoritative source of rule codes, which
/// lives for the process lifetime.
/// Spec: docs/specs/cli.md#warnings-are-typed-values
#[allow(clippy::expect_used)] // check() calls this only for table-owned rule codes.
fn rule_warning_code(code: &str) -> WarningCode {
    let rule = table()
        .iter()
        .find(|rule| rule.code == code)
        .expect("policy code in table");
    WarningCode(rule.code.as_str())
}

/// Position of a layer in the canonical precedence order.
fn layer_rank(layer: Layer) -> usize {
    CANONICAL_ORDER
        .iter()
        .position(|candidate| *candidate == layer)
        .unwrap_or_default()
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            rules: table()
                .iter()
                .cloned()
                .map(|rule| {
                    let source = layer_source(Layer::Default, &rule.code);
                    EffectiveRule { rule, source }
                })
                .collect(),
            settings: Settings::default(),
        }
    }
}
impl Policy {
    /// Apply one layer's policy candidates. Only a `none` row accepts an
    /// explicit `policy.<CODE>`; a boundary or operation code warns with
    /// `CHECK_NOT_OVERRIDABLE` and keeps its compiled strength. The bare key
    /// `default` and any declared group name set `policy.default` or
    /// `policy.<GROUP>` instead: they name no row, so they never raise that
    /// warning and `--checks off` emits no storm of them.
    pub fn overlay(&mut self, values: &BTreeMap<String, Strength>, layer: Layer) {
        for (code, strength) in values {
            let source = layer_source(layer, code);
            let key = format!("policy.{code}");
            if code == "default"
                || table()
                    .iter()
                    .any(|rule| rule.group.as_deref() == Some(code.as_str()))
            {
                self.settings.insert(
                    key,
                    Sourced {
                        value: *strength,
                        source,
                    },
                );
                continue;
            }
            let Some(effective) = self.rules.iter_mut().find(|value| value.rule.code == *code)
            else {
                warning::warn(
                    Warning::new(
                        CONFIG_KEY_IGNORED,
                        format!("policy override {code} ignored: no such rule"),
                        crate::warning::detected_source(CONFIG_KEY_IGNORED),
                    )
                    .subject(code.as_str())
                    .source(source)
                    .repair("use a rule code from the compiled policy table"),
                );
                continue;
            };
            if effective.rule.boundary != Boundary::None {
                warning::warn(
                    Warning::new(
                        CHECK_NOT_OVERRIDABLE,
                        format!(
                            "policy.{code} is not overridable: {:?} rules are immutable",
                            effective.rule.boundary
                        ),
                        crate::warning::detected_source(CHECK_NOT_OVERRIDABLE),
                    )
                    .subject(code.as_str())
                    .source(source.clone())
                    .setting(setting_ref(&key, *strength, &source))
                    .repair(format!(
                        "remove policy.{code}; only none-row checks are configurable"
                    )),
                );
                continue;
            }
            if layer_rank(layer) < layer_rank(effective.source.layer) {
                continue;
            }
            if !effective.rule.supports(*strength) {
                let supported = effective
                    .rule
                    .strengths()
                    .into_iter()
                    .map(strength_name)
                    .collect::<Vec<_>>()
                    .join(", ");
                warning::warn(
                    Warning::new(
                        CONFIG_MALFORMED,
                        format!(
                            "policy override {code}={}: unsupported; supported strengths: {supported}; retaining {} from {:?}",
                            strength_name(*strength),
                            strength_name(effective.rule.strength),
                            effective.source.layer
                        ),
                        crate::warning::detected_source(CONFIG_MALFORMED),
                    )
                    .subject(code.as_str())
                    .source(source)
                    .repair(format!("use one of: {supported}")),
                );
                continue;
            }
            self.settings.insert(
                key,
                Sourced {
                    value: *strength,
                    source: source.clone(),
                },
            );
            effective.rule.strength = *strength;
            effective.source = source;
        }
    }

    /// Set `policy.default` at a layer. `--strict` uses `Refuse` and `--checks
    /// off` uses `Off`, both at the launch layer so they outrank any host
    /// `policy.<CODE>` while still affecting only `refusable` `none` rows.
    pub fn set_default(&mut self, strength: Strength, layer: Layer) {
        self.overlay(&BTreeMap::from([("default".to_owned(), strength)]), layer);
    }

    /// Translate a launch's `--strict` / `--checks` selection. `On` removes a
    /// launch-layer default so the layered configuration applies again.
    pub fn apply_checks(&mut self, mode: ChecksMode) {
        match mode {
            ChecksMode::On => {
                if let Some(candidates) = self.settings.candidates.get_mut("policy.default") {
                    candidates.retain(|candidate| candidate.source.layer != Layer::Launch);
                }
            }
            ChecksMode::Warn => self.set_default(Strength::Warn, Layer::Launch),
            ChecksMode::Strict => self.set_default(Strength::Refuse, Layer::Launch),
            ChecksMode::Off => self.set_default(Strength::Off, Layer::Launch),
        }
    }

    #[allow(clippy::expect_used)] // Enforcement callers use table-owned static codes.
    pub fn rule(&self, code: &str) -> &EffectiveRule {
        self.rules
            .iter()
            .find(|value| value.rule.code == code)
            .expect("policy code in table")
    }
    pub fn check(&self, code: &str, facts: Facts) -> Effect {
        if !facts.violated {
            return Effect::Proceed;
        }
        let effective = self.rule(code);
        let rule = &effective.rule;
        if rule.boundary.is_immutable() {
            // A boundary or operation row never reads a setting and never
            // cites the ADR that defines the three boundaries: proceeding
            // cannot produce a valid result, or the held authority is absent.
            let message = format!(
                "[{code}] {} (boundary={:?}, immutable; no override)",
                facts.detail, rule.boundary
            );
            return Effect::Refuse(Box::new(
                crate::diagnostic::Diagnostic::new(code).detail(message),
            ));
        }
        let (key, resolved) = resolve_setting(code, rule.group.as_deref(), &self.settings);
        // --strict raises only refusable rows; an observational row keeps its
        // compiled strength even when `policy.default` is set to refuse.
        let strength = if resolved.value == Strength::Refuse && !rule.refusable {
            rule.strength
        } else {
            resolved.value
        };
        if strength == Strength::Off {
            return Effect::Proceed;
        }
        let setting = setting_ref(&key, strength, &resolved.source);
        let message = format!(
            "[{code}] {} (strength={}, source={:?}, {}={}, knob={})",
            facts.detail,
            strength_name(strength),
            resolved.source.layer,
            key,
            setting.value,
            rule.knob.as_deref().unwrap_or("immutable; no override")
        );
        match strength {
            Strength::Off => Effect::Proceed,
            Strength::Warn => Effect::Warn(
                Warning::new(
                    rule_warning_code(code),
                    message,
                    crate::warning::detected_source(rule_warning_code(code)),
                )
                .subject(code.to_owned())
                .source(resolved.source)
                .setting(setting)
                // Observational rules warn and proceed: one notice per
                // state change is enough for a person, while the JSON
                // envelope keeps every warning as data.
                .once_per_change(),
            ),
            Strength::Refuse => Effect::Stop(Box::new(
                crate::diagnostic::Diagnostic::new(code).detail(message),
            )),
        }
    }
    /// Shared path for a non-boundary rule: a violated predicate reports a
    /// typed warning through `records::warning`, naming the rule, setting source
    /// and knob, then proceeds; under strict mode the caller's configured stop
    /// is returned. Boundary rules use `boundary_error`, never this.
    #[allow(clippy::panic)]
    pub fn warn_or<E>(
        &self,
        code: &str,
        facts: Facts,
        refuse: impl FnOnce(crate::diagnostic::Diagnostic) -> E,
    ) -> Result<(), E> {
        assert!(
            self.rule(code).rule.boundary == Boundary::None,
            "warn_or serves only non-boundary rules"
        );
        match self.check(code, facts) {
            Effect::Proceed => Ok(()),
            Effect::Warn(w) => {
                warning::warn(w);
                Ok(())
            }
            Effect::Stop(diagnostic) => Err(refuse(*diagnostic)),
            Effect::Refuse(_) => panic!("non-boundary policy cannot refuse"),
        }
    }
    /// Keep an existing domain error while routing its immutable decision
    /// through the compiled policy table. Boundary and operation rules have no
    /// knobs and must always refuse a violated predicate; a table regression
    /// here is a programming error, not a reason to weaken them.
    #[allow(clippy::panic)]
    pub fn boundary_error<E>(&self, code: &str, facts: Facts, typed_error: E) -> E {
        let effective = self.rule(code);
        assert!(
            effective.rule.boundary.is_immutable()
                && effective.rule.knob.is_none()
                && effective.rule.strength == Strength::Refuse,
            "boundary and operation policy must be immutable and refusing"
        );
        match self.check(code, facts) {
            Effect::Refuse(_) => typed_error,
            Effect::Proceed | Effect::Warn(_) | Effect::Stop(_) => {
                panic!("violated boundary policy did not refuse")
            }
        }
    }
    /// Presentation and lane guidance use exactly the resolved enforcement
    /// rows, including a `policy.default` set by `--strict` or `--checks`.
    pub fn render(&self) -> String {
        self.rules
            .iter()
            .map(|value| {
                let (resolved, source) = if value.rule.boundary == Boundary::None {
                    let (_, sourced) = resolve_setting(
                        &value.rule.code,
                        value.rule.group.as_deref(),
                        &self.settings,
                    );
                    (sourced.value, sourced.source)
                } else {
                    (value.rule.strength, value.source.clone())
                };
                let strength = if resolved == Strength::Refuse && !value.rule.refusable {
                    value.rule.strength
                } else {
                    resolved
                };
                format!(
                    "{} boundary={:?} strength={:?} effect={} source={:?} knob={} supported={}\n",
                    value.rule.code,
                    value.rule.boundary,
                    strength,
                    match (value.rule.boundary, strength) {
                        (_, Strength::Off) => "proceed",
                        (_, Strength::Warn) => "warn",
                        (Boundary::None, Strength::Refuse) => "configured_stop",
                        (Boundary::Operation, Strength::Refuse) => "operation_error",
                        (_, Strength::Refuse) => "boundary_refusal",
                    },
                    source,
                    value.rule.knob.as_deref().unwrap_or("none"),
                    value
                        .rule
                        .strengths()
                        .into_iter()
                        .map(strength_name)
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect()
    }
}

const fn strength_name(strength: Strength) -> &'static str {
    match strength {
        Strength::Refuse => "refuse",
        Strength::Warn => "warn",
        Strength::Off => "off",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_guidance_tracks_every_supported_strength() {
        for rule in table() {
            let behaviour = rule.behaviour();
            for strength in Strength::ALL {
                assert_eq!(
                    behaviour.contains(&format!("{}:", strength_name(strength))),
                    rule.supports(strength),
                    "{}",
                    rule.code
                );
                if rule.supports(strength) {
                    assert!(behaviour.contains(strength.behaviour()));
                }
            }
        }
    }

    #[test]
    fn non_boundary_defaults_never_refuse() {
        for rule in table() {
            if rule.boundary == Boundary::None {
                assert_ne!(rule.strength, Strength::Refuse, "{}", rule.code);
                assert!(rule.knob.is_some(), "{}", rule.code);
                assert!(rule.supports(Strength::Warn), "{}", rule.code);
                assert!(rule.supports(Strength::Off), "{}", rule.code);
                assert_eq!(
                    rule.supports(Strength::Refuse),
                    rule.refusable,
                    "{}",
                    rule.code
                );
            } else {
                assert_eq!(rule.strength, Strength::Refuse, "{}", rule.code);
                assert!(rule.knob.is_none(), "{}", rule.code);
                assert!(rule.supports(Strength::Refuse), "{}", rule.code);
                assert!(!rule.supports(Strength::Warn), "{}", rule.code);
                assert!(!rule.supports(Strength::Off), "{}", rule.code);
            }
        }
    }
    #[test]
    fn rules_guarding_no_boundary_warn_by_default_and_document_their_knob() {
        let registry: serde_json::Value =
            serde_json::from_str(include_str!("../../../../../config/harness-knobs.json"))
                .expect("knob registry");
        let documented = registry["policy_rules"]["settings"]
            .as_object()
            .expect("policy_rules settings");
        // Opt-in overlap noise; the sole deliberate non-warn default.
        const QUIET: &str = "CONFINEMENT_SOURCE_AGENT_CONFIG_NONE";
        for rule in table()
            .iter()
            .filter(|rule| rule.boundary == Boundary::None)
        {
            let expected = if rule.code == QUIET {
                Strength::Off
            } else {
                Strength::Warn
            };
            assert_eq!(rule.strength, expected, "{}", rule.code);
            let knob = rule.knob.as_deref().expect("knob");
            let entry = documented
                .get(knob)
                .unwrap_or_else(|| panic!("{knob} undocumented in harness-knobs.json"));
            assert_eq!(entry["default"], strength_name(rule.strength), "{knob}");
        }
    }

    #[test]
    fn warn_or_warns_by_default_and_refuses_under_strict_mode() {
        let mut policy = Policy::default();
        let code = "LANE_DEPTH";
        assert!(
            table()
                .iter()
                .any(|rule| rule.code == code && rule.refusable)
        );
        let fact = || Facts::violation("depth 5 exceeds 4");
        assert!(policy.warn_or(code, fact(), |_| ()).is_ok());
        policy.overlay(
            &BTreeMap::from([(code.into(), Strength::Refuse)]),
            Layer::Project,
        );
        let diagnostic = policy
            .warn_or(code, fact(), |diagnostic| diagnostic)
            .expect_err("strict mode refuses");
        assert!(
            diagnostic
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("Project"))
        );
    }
    /// Spec: docs/specs/cli.md#warnings-are-typed-values
    #[test]
    fn a_violated_rule_warns_under_its_own_code_not_a_catch_all() {
        let policy = Policy::default();
        let ((), warnings) = crate::warning::capture(|| {
            assert!(
                policy
                    .warn_or("LANE_DEPTH", Facts::violation("depth 5 exceeds 4"), |_| ())
                    .is_ok()
            );
            assert!(
                policy
                    .warn_or(
                        "PR_ATTEMPT_STATE",
                        Facts::violation("attempt a1 ended failed"),
                        |_| ()
                    )
                    .is_ok()
            );
        });
        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[0].code.as_ref(), "LANE_DEPTH");
        assert_eq!(warnings[1].code.as_ref(), "PR_ATTEMPT_STATE");
        assert_ne!(warnings[0].code, warnings[1].code);
    }

    /// A policy warning is observational: it names the layer that set the rule
    /// and asks to be shown once per change, while the JSON envelope keeps it.
    /// Spec: docs/specs/cli.md#warnings-are-typed-values
    #[test]
    fn a_policy_warning_carries_the_rule_source_and_dedups_per_change() {
        let policy = Policy::default();
        let Effect::Warn(warning) =
            policy.check("WRITER_CENSUS", Facts::violation("census incomplete"))
        else {
            panic!("an observational rule must warn")
        };
        assert_eq!(warning.code.as_ref(), "WRITER_CENSUS");
        assert_eq!(warning.subject.as_deref(), Some("WRITER_CENSUS"));
        assert_eq!(warning.dedup, crate::warning::Dedup::PerChange);
        let source = warning.source;
        assert_eq!(source.layer, Layer::Default);
        assert_eq!(source.origin.as_deref(), Some("compiled policy table"));

        let mut overlaid = Policy::default();
        overlaid.overlay(
            &BTreeMap::from([("WRITER_CENSUS".into(), Strength::Off)]),
            Layer::Host,
        );
        overlaid.overlay(
            &BTreeMap::from([("WRITER_CENSUS".into(), Strength::Warn)]),
            Layer::Project,
        );
        let Effect::Warn(warning) =
            overlaid.check("WRITER_CENSUS", Facts::violation("census incomplete"))
        else {
            panic!("an observational rule must warn")
        };
        let source = warning.source;
        assert_eq!(source.layer, Layer::Project);
        assert_eq!(
            source.origin.as_deref(),
            Some(".provenant/policy.toml#policy.WRITER_CENSUS")
        );
    }

    #[test]
    fn opening_a_pr_from_an_unsettled_attempt_warns_and_can_be_made_a_stop() {
        let mut policy = Policy::default();
        let fact = || Facts::violation("attempt a1 ended failed");
        assert!(policy.warn_or("PR_ATTEMPT_STATE", fact(), |_| ()).is_ok());
        policy.overlay(
            &BTreeMap::from([("PR_ATTEMPT_STATE".into(), Strength::Refuse)]),
            Layer::Host,
        );
        assert!(policy.warn_or("PR_ATTEMPT_STATE", fact(), |_| ()).is_err());
    }
    #[test]
    #[allow(clippy::panic)]
    fn overlays_report_source_and_do_not_change_boundary_authority() {
        let mut policy = Policy::default();
        for (source, strength) in [
            (Layer::Host, Strength::Off),
            (Layer::Project, Strength::Warn),
            (Layer::Route, Strength::Refuse),
        ] {
            policy.overlay(&BTreeMap::from([("WRITE_SCOPE".into(), strength)]), source);
            assert_eq!(policy.rule("WRITE_SCOPE").source.layer, source);
            assert_eq!(policy.rule("WRITE_SCOPE").rule.strength, strength);
        }
        policy.overlay(
            &BTreeMap::from([("WRITE_SCOPE".into(), Strength::Off)]),
            Layer::Host,
        );
        assert_eq!(policy.rule("WRITE_SCOPE").source.layer, Layer::Route);
        assert_eq!(policy.rule("WRITE_SCOPE").rule.strength, Strength::Refuse);
        let Effect::Stop(diagnostic) =
            policy.check("WRITE_SCOPE", Facts::violation("outside scope"))
        else {
            panic!("strict policy must refuse")
        };
        assert_eq!(diagnostic.code, "WRITE_SCOPE");
        for rule in table()
            .iter()
            .filter(|rule| rule.boundary != Boundary::None)
        {
            policy.overlay(
                &BTreeMap::from([(rule.code.clone(), Strength::Off)]),
                Layer::Route,
            );
            assert_eq!(policy.rule(&rule.code).rule.strength, Strength::Refuse);
        }
    }

    fn sourced(value: Strength, layer: Layer) -> Sourced<Strength> {
        Sourced {
            value,
            source: ProvenanceSource {
                layer,
                origin: Some(format!("test#{layer}")),
            },
        }
    }

    #[test]
    fn higher_layer_beats_more_specific_key() {
        let mut settings = Settings::default();
        // A launch default outranks a host `policy.CODE`, even though the code
        // key is more specific.
        settings.insert("policy.default", sourced(Strength::Off, Layer::Launch));
        settings.insert("policy.GRANT_COUNT", sourced(Strength::Refuse, Layer::Host));
        let (key, resolved) = resolve_setting("GRANT_COUNT", None, &settings);
        assert_eq!(key, "policy.default");
        assert_eq!(resolved.value, Strength::Off);
        assert_eq!(resolved.source.layer, Layer::Launch);

        // Within one layer, code beats group beats default.
        let mut settings = Settings::default();
        settings.insert("policy.default", sourced(Strength::Refuse, Layer::Host));
        settings.insert("policy.custody", sourced(Strength::Off, Layer::Host));
        settings.insert("policy.WRITE_SCOPE", sourced(Strength::Warn, Layer::Host));
        let (key, resolved) = resolve_setting("WRITE_SCOPE", Some("custody"), &settings);
        assert_eq!(key, "policy.WRITE_SCOPE");
        assert_eq!(resolved.value, Strength::Warn);
        let (key, resolved) = resolve_setting("WRITER_CENSUS", Some("custody"), &settings);
        assert_eq!(key, "policy.custody");
        assert_eq!(resolved.value, Strength::Off);
    }

    #[test]
    fn strict_raises_only_refusable_rows() {
        let mut policy = Policy::default();
        policy.apply_checks(ChecksMode::Strict);
        let refusable = table()
            .iter()
            .find(|rule| rule.boundary == Boundary::None && rule.refusable)
            .expect("a refusable row");
        assert!(matches!(
            policy.check(&refusable.code, Facts::violation("observed")),
            Effect::Stop(_)
        ));
        let observational = table()
            .iter()
            .find(|rule| rule.boundary == Boundary::None && !rule.refusable)
            .expect("an observational row");
        assert!(!matches!(
            policy.check(&observational.code, Facts::violation("observed")),
            Effect::Stop(_)
        ));
    }

    #[test]
    fn checks_off_emits_no_not_overridable_storm() {
        let mut policy = Policy::default();
        policy.apply_checks(ChecksMode::Off);
        let ((), warnings) = crate::warning::capture(|| {
            for rule in table()
                .iter()
                .filter(|rule| rule.boundary == Boundary::None)
            {
                assert!(matches!(
                    policy.check(&rule.code, Facts::violation("observed")),
                    Effect::Proceed
                ));
            }
        });
        assert!(
            warnings
                .iter()
                .all(|warning| warning.code.as_ref() != CHECK_NOT_OVERRIDABLE.as_str()),
            "{warnings:?}"
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn code_on_boundary_row_warns_not_overridable() {
        let mut policy = Policy::default();
        let code = table()
            .iter()
            .find(|rule| rule.boundary == Boundary::Authority)
            .map(|rule| rule.code.clone())
            .expect("an authority row");
        let compiled = policy.rule(&code).rule.strength;
        let ((), warnings) = crate::warning::capture(|| {
            policy.overlay(
                &BTreeMap::from([(code.clone(), Strength::Off)]),
                Layer::Host,
            );
        });
        assert_eq!(policy.rule(&code).rule.strength, compiled);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.code.as_ref() == CHECK_NOT_OVERRIDABLE.as_str()),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|warning| warning.setting.is_some()),
            "{warnings:?}"
        );
    }

    #[test]
    fn every_refuse_row_maps_to_three_boundaries_or_operation() {
        for rule in table() {
            if rule.strength == Strength::Refuse {
                assert!(rule.boundary.is_immutable(), "{}", rule.code);
            }
        }
    }

    #[test]
    fn operation_rows_never_cite_adr_0035() {
        for rule in table()
            .iter()
            .filter(|rule| rule.boundary == Boundary::Operation)
        {
            assert!(!rule.justification.contains("0035"), "{}", rule.code);
        }
    }

    #[test]
    fn sourced_settings_use_provenance_layer() {
        // Resolution returns the shared provenance type, not a family-local
        // source enum, so every layer has the same spelling and precedence.
        let resolved: Sourced<Strength> =
            resolve_strength("WRITE_SCOPE", None, &Settings::default());
        assert_eq!(resolved.source.layer, Layer::Default);
        assert!(resolved.source.origin.is_some());
    }

    /// Every default rule gets its provenance from the compiled table. The
    /// source is read from the rule before the rule moves into the entry, so
    /// no field is borrowed after the move.
    #[test]
    fn default_rules_carry_the_compiled_table_source() {
        let policy = Policy::default();
        assert_eq!(policy.rules.len(), table().len());
        for effective in &policy.rules {
            assert_eq!(
                effective.source.layer,
                Layer::Default,
                "{}",
                effective.rule.code
            );
            assert_eq!(
                effective.source.origin.as_deref(),
                Some("compiled policy table"),
                "{}",
                effective.rule.code
            );
        }
    }
}
