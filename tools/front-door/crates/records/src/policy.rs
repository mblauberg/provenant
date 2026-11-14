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
            Self::Refuse => "refuse the violated predicate",
            Self::Warn => "warn and proceed",
            Self::Off => "proceed without a policy warning",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    CompiledDefault,
    Host,
    Project,
    Dispatch,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rule {
    pub code: String,
    pub boundary: Boundary,
    pub strength: Strength,
    pub knob: Option<String>,
    pub justification: String,
    pub site: String,
    pub threshold: Option<String>,
    /// A non-boundary rule may offer a strict, user-selected refusal only
    /// when its consumer has a meaningful refusal path.
    #[serde(default)]
    pub refusable: bool,
}
impl Rule {
    pub fn supports(&self, strength: Strength) -> bool {
        match (self.boundary, strength) {
            (Boundary::None, Strength::Refuse) => self.refusable,
            (Boundary::None, Strength::Warn | Strength::Off) => true,
            (_, Strength::Refuse) => true,
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
            "Policy {} (default={}, source=compiled default; override={}): {}.",
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectiveRule {
    #[serde(flatten)]
    pub rule: Rule,
    pub source: Source,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Policy {
    pub rules: Vec<EffectiveRule>,
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
    Refuse(crate::diagnostic::Diagnostic),
}
impl Effect {
    pub fn blocks(&self) -> bool {
        matches!(self, Self::Refuse(_))
    }
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Proceed => None,
            Self::Warn(message) => Some(message),
            Self::Refuse(diag) => diag.detail.as_deref(),
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
            Strength::Refuse => {
                Effect::Refuse(crate::diagnostic::Diagnostic::new(code).detail(message))
            }
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
            Effect::Proceed | Effect::Warn(_) => {
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
                    "{} boundary={:?} strength={:?} source={:?} knob={} supported={}\n",
                    value.rule.code,
                    value.rule.boundary,
                    value.rule.strength,
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
        let Effect::Refuse(diagnostic) =
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
