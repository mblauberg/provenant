//! Host/project policy overlays; the executable defaults belong to records.
use crate::{DomainConfig, DomainSection, FieldMetadata};
use provenant_records::policy::{Policy, Source, Strength};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub projects: BTreeMap<String, Project>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Project {
    pub rules: BTreeMap<String, Strength>,
}
impl DomainConfig for Settings {
    fn validate(&self) -> Vec<crate::SectionError> {
        let mut errors = Vec::new();
        for (project, settings) in &self.projects {
            for (code, strength) in &settings.rules {
                let field = format!("projects.{project}.rules.{code}");
                let Some(rule) = provenant_records::policy::table()
                    .iter()
                    .find(|rule| rule.code == *code)
                else {
                    errors.push(crate::SectionError {
                        field,
                        code: "unknown_policy_rule",
                        detail: Some("policy rule is not in the compiled table"),
                    });
                    continue;
                };
                if !rule.supports(*strength) {
                    errors.push(crate::SectionError {
                        field,
                        code: "unsupported_policy_strength",
                        detail: Some("strength is not supported by this policy rule"),
                    });
                }
            }
        }
        errors
    }
}
pub const SECTION: DomainSection =
    DomainSection::new::<Settings>("policy", |_| "[projects]\n").with_fields(&[
        FieldMetadata::new("projects.{project}.rules.{code}", "string"),
    ]);
impl Settings {
    pub fn resolve_with_knobs(
        &self,
        project: &str,
        knobs: &BTreeMap<String, crate::KnobValue>,
        dispatch: &BTreeMap<String, Strength>,
    ) -> Policy {
        let mut policy = Policy::default();
        overlay_knobs(&mut policy, knobs);
        if let Some(project) = self.projects.get(project) {
            policy.overlay(&project.rules, Source::Project);
        }
        policy.overlay(dispatch, Source::Dispatch);
        policy
    }
    pub fn resolve(&self, project: &str, dispatch: &BTreeMap<String, Strength>) -> Policy {
        let mut policy = Policy::default();
        if let Some(project) = self.projects.get(project) {
            policy.overlay(&project.rules, Source::Project);
        }
        policy.overlay(dispatch, Source::Dispatch);
        policy
    }
}
/// Resolve one immutable launch snapshot. Invalid optional policy settings warn
/// and retain compiled defaults, without changing the four boundaries.
pub fn read(state: &Path, project: &str, dispatch: &BTreeMap<String, Strength>) -> Policy {
    use std::io::Write;
    let settings = crate::read_domain::<Settings>(state, "policy").unwrap_or_else(|error| {
        let _ = writeln!(
            std::io::stderr().lock(),
            "policy settings unavailable ({error}); using compiled defaults"
        );
        Settings::default()
    });
    let mut policy = Policy::default();
    if let Ok(knobs) =
        crate::loader::read_boundary_settings::<BTreeMap<String, serde_json::Value>>(state, "knobs")
    {
        let mut values = BTreeMap::new();
        for (name, value) in knobs {
            if let Some(code) = name.strip_prefix("policy.")
                && let Ok(strength) = serde_json::from_value(value)
            {
                values.insert(code.to_owned(), strength);
            }
        }
        policy.overlay(&values, Source::Host);
    }
    if let Some(project) = settings.projects.get(project) {
        policy.overlay(&project.rules, Source::Project);
    }
    policy.overlay(dispatch, Source::Dispatch);
    policy
}
/// Dispatch overrides are already parsed and sealed by `dispatch_profile`.
pub fn dispatch(profile: &provenant_records::run::HarnessProfile) -> BTreeMap<String, Strength> {
    profile
        .overrides
        .iter()
        .filter_map(|entry| {
            let (name, _) = entry.split_once('=')?;
            let code = name.strip_prefix("policy.")?;
            let strength = serde_json::from_value(profile.knobs.get(name)?.clone()).ok()?;
            Some((code.to_owned(), strength))
        })
        .collect()
}
pub fn for_task(state: &Path, project: &str, task: &provenant_records::run::TaskRequest) -> Policy {
    if let Some(value) = task.profile.extra.get("launch_policy")
        && let Ok(snapshot) = serde_json::from_value::<Policy>(value.clone())
    {
        // Only configurable strengths can be restored. Persisted data cannot
        // replace table-owned boundaries, codes, knobs or justifications.
        let mut policy = Policy::default();
        for row in snapshot.rules {
            let Some(table_rule) = policy
                .rules
                .iter()
                .find(|effective| effective.rule.code == row.rule.code)
            else {
                continue;
            };
            if table_rule.rule.knob.is_none() {
                continue;
            }
            policy.overlay(
                &BTreeMap::from([(row.rule.code, row.rule.strength)]),
                row.source,
            );
        }
        return policy;
    }
    let mut values = dispatch(&task.profile);
    if let Some(value) = task.profile.extra.get("policy_overrides")
        && let Ok(overrides) = serde_json::from_value::<BTreeMap<String, Strength>>(value.clone())
    {
        values.extend(overrides);
    }
    read(state, project, &values)
}

pub fn overlay_knobs(policy: &mut Policy, knobs: &BTreeMap<String, crate::KnobValue>) {
    let values = knobs
        .iter()
        .filter_map(|(name, knob)| {
            if knob.source != crate::KnobSource::Override {
                return None;
            }
            Some((
                name.strip_prefix("policy.")?.to_owned(),
                serde_json::from_value(knob.value.clone()).ok()?,
            ))
        })
        .collect();
    policy.overlay(&values, Source::Host);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_rules_reject_unsupported_strengths_and_unknown_codes() {
        let settings = Settings {
            projects: BTreeMap::from([(
                "project".into(),
                Project {
                    rules: BTreeMap::from([
                        ("PROCESS_CENSUS".into(), Strength::Refuse),
                        ("MISSING_RULE".into(), Strength::Off),
                    ]),
                },
            )]),
        };
        let errors = settings.validate();
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().any(|error| {
            error.field == "projects.project.rules.PROCESS_CENSUS"
                && error.code == "unsupported_policy_strength"
        }));
        assert!(errors.iter().any(|error| {
            error.field == "projects.project.rules.MISSING_RULE"
                && error.code == "unknown_policy_rule"
        }));
    }
}
