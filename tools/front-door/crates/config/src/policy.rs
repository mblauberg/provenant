//! Host/project policy overlays; the executable defaults belong to records.
use crate::{DomainConfig, DomainSection, FieldMetadata};
use provenant_records::policy::{Policy, Source, Strength};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub rules: BTreeMap<String, Strength>,
    pub projects: BTreeMap<String, Project>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Project {
    pub rules: BTreeMap<String, Strength>,
}
impl DomainConfig for Settings {}
pub const SECTION: DomainSection =
    DomainSection::new::<Settings>("policy", |_| "[rules]\n[projects]\n").with_fields(&[
        FieldMetadata::enumeration("rules.{code}", &["refuse", "queue", "warn", "off"]),
        FieldMetadata::enumeration(
            "projects.{project}.rules.{code}",
            &["refuse", "queue", "warn", "off"],
        ),
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
        policy.overlay(&self.rules, Source::Host);
        if let Some(project) = self.projects.get(project) {
            policy.overlay(&project.rules, Source::Project);
        }
        policy.overlay(dispatch, Source::Dispatch);
        policy
    }
    pub fn resolve(&self, project: &str, dispatch: &BTreeMap<String, Strength>) -> Policy {
        let mut policy = Policy::default();
        policy.overlay(&self.rules, Source::Host);
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
            if name == "policy" {
                if let Ok(nested) = serde_json::from_value::<BTreeMap<String, Strength>>(value) {
                    values.extend(nested);
                }
            } else if let Some(code) = name.strip_prefix("policy.")
                && let Ok(strength) = serde_json::from_value(value)
            {
                values.insert(code.to_owned(), strength);
            }
        }
        policy.overlay(&values, Source::Host);
    }
    policy.overlay(&settings.rules, Source::Host);
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
