//! One executable policy table, shared by enforcement and presentation.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Boundary {
    Credentials,
    GitIsolation,
    DataIntegrity,
    Authority,
    None,
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
                | Boundary::GitIsolation
                | Boundary::DataIntegrity
                | Boundary::Authority,
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
            "Policy {} (default={}, source=compiled default; override={}): {}. For a non-boundary rule, refuse means a configured stop; boundary refusal is immutable.",
            self.code,
            strength_name(self.strength),
            self.knob.as_deref().unwrap_or("none; immutable boundary"),
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
    Warn(String),
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
            Self::Warn(message) => Some(message),
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
    #[allow(clippy::print_stderr)]
    pub fn overlay(&mut self, values: &BTreeMap<String, Strength>, source: Source) {
        use std::io::Write;
        for (code, strength) in values {
            let Some(effective) = self.rules.iter_mut().find(|value| value.rule.code == *code)
            else {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "policy override {code} ignored: no such rule"
                );
                continue;
            };
            if source < effective.source {
                continue;
            }
            if effective.rule.knob.is_none() {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "policy override {code} ignored: boundary rules have no override"
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
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "policy override {code}={}: unsupported; supported strengths: {supported}; retaining {} from {:?}",
                    strength_name(*strength),
                    strength_name(effective.rule.strength),
                    effective.source
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
            rule.knob.as_deref().unwrap_or("boundary; no override")
        );
        match rule.strength {
            Strength::Off => Effect::Proceed,
            Strength::Warn => Effect::Warn(message),
            Strength::Refuse if rule.boundary == Boundary::None => Effect::Stop(Box::new(
                crate::diagnostic::Diagnostic::new(code).detail(message),
            )),
            Strength::Refuse => Effect::Refuse(Box::new(
                crate::diagnostic::Diagnostic::new(code).detail(message),
            )),
        }
    }
    /// Shared path for a non-boundary rule: a violated predicate warns on
    /// stderr, naming the rule, setting source and knob, then proceeds; under
    /// strict mode the caller's configured stop is returned. Boundary rules use
    /// `boundary_error`, never this.
    #[allow(clippy::print_stderr, clippy::panic)]
    pub fn warn_or<E>(
        &self,
        code: &str,
        facts: Facts,
        refuse: impl FnOnce(crate::diagnostic::Diagnostic) -> E,
    ) -> Result<(), E> {
        use std::io::Write;
        assert!(
            self.rule(code).rule.boundary == Boundary::None,
            "warn_or serves only non-boundary rules"
        );
        match self.check(code, facts) {
            Effect::Proceed => Ok(()),
            Effect::Warn(message) => {
                let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
                Ok(())
            }
            Effect::Stop(diagnostic) => Err(refuse(*diagnostic)),
            Effect::Refuse(_) => panic!("non-boundary policy cannot refuse"),
        }
    }
    /// Keep an existing domain error while routing its immutable safety
    /// decision through the compiled policy table. Boundary rules have no
    /// knobs and must always refuse a violated predicate; a table regression
    /// here is a programming error, not a reason to weaken the boundary.
    #[allow(clippy::panic)]
    pub fn boundary_error<E>(&self, code: &str, facts: Facts, typed_error: E) -> E {
        let effective = self.rule(code);
        assert!(
            effective.rule.boundary != Boundary::None
                && effective.rule.knob.is_none()
                && effective.rule.strength == Strength::Refuse,
            "boundary policy must be immutable and refusing"
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
