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
    Queue,
    Warn,
    Off,
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
    Queue(String),
    Refuse(crate::diagnostic::Diagnostic),
}
impl Effect {
    pub fn blocks(&self) -> bool {
        matches!(self, Self::Queue(_) | Self::Refuse(_))
    }
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Proceed => None,
            Self::Warn(message) | Self::Queue(message) => Some(message),
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
    pub fn overlay(&mut self, values: &BTreeMap<String, Strength>, source: Source) {
        for effective in &mut self.rules {
            if effective.rule.knob.is_some()
                && let Some(strength) = values.get(&effective.rule.code)
            {
                effective.rule.strength = *strength;
                effective.source = source;
            }
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
            Strength::Queue => Effect::Queue(message),
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
            Effect::Proceed | Effect::Warn(_) | Effect::Queue(_) => {
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
                    "{} boundary={:?} strength={:?} source={:?} knob={}\n",
                    value.rule.code,
                    value.rule.boundary,
                    value.rule.strength,
                    value.source,
                    value.rule.knob.as_deref().unwrap_or("none")
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn non_boundary_defaults_never_refuse() {
        for rule in table() {
            if rule.boundary == Boundary::None {
                assert_ne!(rule.strength, Strength::Refuse, "{}", rule.code);
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
