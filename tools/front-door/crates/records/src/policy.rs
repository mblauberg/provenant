//! One executable policy table, shared by enforcement and presentation.
use crate::provenance::{Layer, Source as ProvenanceSource};
use crate::warning::{self, CONFIG_KEY_IGNORED, CONFIG_MALFORMED, Warning, WarningCode};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    CompiledDefault,
    Host,
    Project,
    Dispatch,
}
impl Source {
    /// The shared provenance layer for this policy layer. A dispatch override
    /// arrives with the resolved route's policy.
    pub const fn layer(self) -> crate::Layer {
        match self {
            Self::CompiledDefault => crate::Layer::Default,
            Self::Host => crate::Layer::Host,
            Self::Project => crate::Layer::Project,
            Self::Dispatch => crate::Layer::Route,
        }
    }
}

impl Source {
    /// The provenance a warning from this effective rule carries, so a
    /// consumer can see which layer set the rule that warned. The code names
    /// the exact knob or table row; the layer uses the shared vocabulary.
    pub fn provenance(self, code: &str) -> ProvenanceSource {
        let (layer, origin) = match self {
            Self::CompiledDefault => (Layer::Default, "compiled policy table".to_owned()),
            Self::Host => (Layer::Host, format!("config.toml#policy.{code}")),
            Self::Project => (
                Layer::Project,
                format!(".provenant/policy.toml#policy.{code}"),
            ),
            Self::Dispatch => (Layer::Launch, format!("dispatch#policy.{code}")),
        };
        ProvenanceSource {
            layer,
            origin: Some(origin),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub code: String,
    pub boundary: Boundary,
    pub strength: Strength,
    pub knob: Option<String>,
    pub justification: String,
    pub site: String,
    pub threshold: Option<String>,
    /// A non-boundary rule may offer a strict, user-selected stop only
    /// when its consumer has a meaningful stop path.
    #[serde(default)]
    pub refusable: bool,
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
    pub source: Source,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub rules: Vec<EffectiveRule>,
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

impl Default for Policy {
    fn default() -> Self {
        Self {
            rules: table()
                .iter()
                .cloned()
                .map(|rule| EffectiveRule {
                    rule,
                    source: Source::CompiledDefault,
                })
                .collect(),
        }
    }
}
impl Policy {
    /// Apply layers in increasing precedence: host, project, then dispatch.
    /// Boundary rules have no knob, so these overlays cannot grant authority.
    pub fn overlay(&mut self, values: &BTreeMap<String, Strength>, source: Source) {
        for (code, strength) in values {
            let Some(effective) = self.rules.iter_mut().find(|value| value.rule.code == *code)
            else {
                warning::warn(
                    Warning::new(
                        CONFIG_KEY_IGNORED,
                        format!("policy override {code} ignored: no such rule"),
                        crate::warning::detected_source(CONFIG_KEY_IGNORED),
                    )
                    .subject(code.as_str())
                    .source(source.provenance(code))
                    .repair("use a rule code from the compiled policy table"),
                );
                continue;
            };
            if source < effective.source {
                continue;
            }
            if effective.rule.knob.is_none() {
                warning::warn(
                    Warning::new(
                        CONFIG_KEY_IGNORED,
                        format!(
                            "policy override {code} ignored: boundary and operation rules have no override"
                        ),
                     crate::warning::detected_source(CONFIG_KEY_IGNORED))
                    .subject(code.as_str())
                    .source(source.provenance(code))
                    .repair("remove the override; boundary and operation rules have no knob"),
                );
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
                            effective.source
                        ),
                     crate::warning::detected_source(CONFIG_MALFORMED))
                    .subject(code.as_str())
                    .source(source.provenance(code))
                    .repair(format!("use one of: {supported}")),
                );
                continue;
            }
            effective.rule.strength = *strength;
            effective.source = source;
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
        if rule.strength == Strength::Off {
            return Effect::Proceed;
        }
        let message = format!(
            "[{}] {} (strength={:?}, source={:?}, knob={})",
            rule.code,
            facts.detail,
            rule.strength,
            effective.source,
            rule.knob.as_deref().unwrap_or("immutable; no override")
        );
        match rule.strength {
            Strength::Off => Effect::Proceed,
            Strength::Warn => Effect::Warn(
                Warning::new(
                    rule_warning_code(code),
                    message,
                    crate::warning::detected_source(rule_warning_code(code)),
                )
                .subject(code.to_owned())
                .source(effective.source.provenance(code))
                // Observational rules warn and proceed: one notice per
                // state change is enough for a person, while the JSON
                // envelope keeps every warning as data.
                .once_per_change(),
            ),
            Strength::Refuse if rule.boundary == Boundary::None => Effect::Stop(Box::new(
                crate::diagnostic::Diagnostic::new(code).detail(message),
            )),
            Strength::Refuse => Effect::Refuse(Box::new(
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
    /// Presentation and lane guidance use exactly the resolved enforcement rows.
    pub fn render(&self) -> String {
        self.rules
            .iter()
            .map(|value| {
                format!(
                    "{} boundary={:?} strength={:?} effect={} source={:?} knob={} supported={}\n",
                    value.rule.code,
                    value.rule.boundary,
                    value.rule.strength,
                    match (value.rule.boundary, value.rule.strength) {
                        (_, Strength::Off) => "proceed",
                        (_, Strength::Warn) => "warn",
                        (Boundary::None, Strength::Refuse) => "configured_stop",
                        (Boundary::Operation, Strength::Refuse) => "operation_error",
                        (_, Strength::Refuse) => "boundary_refusal",
                    },
                    value.source,
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
            Source::Project,
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
            Source::Host,
        );
        overlaid.overlay(
            &BTreeMap::from([("WRITER_CENSUS".into(), Strength::Warn)]),
            Source::Project,
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
            Source::Host,
        );
        assert!(policy.warn_or("PR_ATTEMPT_STATE", fact(), |_| ()).is_err());
    }
    #[test]
    #[allow(clippy::panic)]
    fn overlays_report_source_and_do_not_change_boundary_authority() {
        let mut policy = Policy::default();
        for (source, strength) in [
            (Source::Host, Strength::Off),
            (Source::Project, Strength::Warn),
            (Source::Dispatch, Strength::Refuse),
        ] {
            policy.overlay(&BTreeMap::from([("WRITE_SCOPE".into(), strength)]), source);
            assert_eq!(policy.rule("WRITE_SCOPE").source, source);
            assert_eq!(policy.rule("WRITE_SCOPE").rule.strength, strength);
        }
        policy.overlay(
            &BTreeMap::from([("WRITE_SCOPE".into(), Strength::Off)]),
            Source::Host,
        );
        assert_eq!(policy.rule("WRITE_SCOPE").source, Source::Dispatch);
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
                Source::Dispatch,
            );
            assert_eq!(policy.rule(&rule.code).rule.strength, Strength::Refuse);
        }
    }
}
