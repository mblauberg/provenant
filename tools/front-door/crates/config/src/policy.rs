//! Host/project policy overlays; the executable defaults belong to records.
use crate::{DomainConfig, DomainSection, FieldMetadata};
use provenant_records::Layer;
use provenant_records::policy::{Policy, Strength};
use provenant_records::warning::{CONFIG_UNAVAILABLE, Warning, warn};
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
        knobs: &BTreeMap<String, provenant_records::Sourced<serde_json::Value>>,
        dispatch: &BTreeMap<String, Strength>,
    ) -> Policy {
        let mut policy = Policy::default();
        overlay_knobs(&mut policy, knobs);
        if let Some(project) = self.projects.get(project) {
            policy.overlay(&project.rules, Layer::Project);
        }
        policy.overlay(dispatch, Layer::Route);
        policy
    }
    pub fn resolve(&self, project: &str, dispatch: &BTreeMap<String, Strength>) -> Policy {
        let mut policy = Policy::default();
        if let Some(project) = self.projects.get(project) {
            policy.overlay(&project.rules, Layer::Project);
        }
        policy.overlay(dispatch, Layer::Route);
        policy
    }
}
/// A single configured policy candidate, shared by enforcement and explanation.
pub(crate) struct Source {
    pub key: String,
    pub strength: Strength,
    pub layer: Layer,
    pub origin: String,
}

pub(crate) fn sources(state: &Path, project: &str, checkout: Option<&Path>) -> Vec<Source> {
    let mut result = Vec::new();
    let mut add = |values: Vec<(String, serde_json::Value)>, layer, origin: &str| {
        for (key, value) in values {
            if let Ok(strength) = serde_json::from_value(value) {
                result.push(Source {
                    origin: format!("{origin}.{key}"),
                    key,
                    strength,
                    layer,
                });
            }
        }
    };
    let host = crate::loader::open_at_state(state)
        .ok()
        .flatten()
        .and_then(|file| crate::loader::bounded_text(file, &state.join("config.toml")).ok())
        .filter(|text| {
            text.parse::<toml_edit::DocumentMut>().ok().and_then(|doc| {
                doc.get("schema_version")
                    .and_then(toml_edit::Item::as_integer)
            }) == Some(crate::CONFIG_SCHEMA_VERSION as i64)
        });
    if let Some(text) = host {
        add(
            crate::inspection::section_values(&text, "knobs")
                .into_iter()
                .filter_map(|(key, value)| Some((key.strip_prefix("policy.")?.to_owned(), value)))
                .collect(),
            Layer::Host,
            "config.toml#knobs.policy",
        );
        add(
            crate::inspection::section_values(&text, "policy"),
            Layer::Host,
            "config.toml#policy",
        );
    }
    if let Ok(settings) = crate::read_domain::<Settings>(state, "policy")
        && let Some(project) = settings.projects.get(project)
    {
        add(
            project
                .rules
                .iter()
                .map(|(key, value)| (key.clone(), serde_json::json!(value)))
                .collect(),
            Layer::Project,
            "domains.policy#projects.rules",
        );
    }
    if let Some(text) = crate::inspection::project_text(checkout, "policy.toml") {
        add(
            crate::inspection::section_values(&text, "policy"),
            Layer::Project,
            ".provenant/policy.toml#policy",
        );
    }
    result
}

pub(crate) fn project_checkout(state: &Path, project: &str) -> Option<std::path::PathBuf> {
    let id = project.parse::<provenant_core::ProjectId>().ok()?;
    let binding = crate::project_binding::lookup_project_by_id(state, id)
        .ok()
        .flatten()?;
    let root = binding.primary_checkout_hint()?;
    let marker = provenant_os::git::repository_marker(root)?;
    let common = std::fs::canonicalize(marker.common_dir?).ok()?;
    (common == binding.common_dir()).then(|| root.to_path_buf())
}

/// Resolve current host and project policy overlaid with the request's sealed
/// dispatch overrides. Invalid optional policy settings warn and retain compiled
/// defaults, without changing the boundaries or operation errors.
pub fn read(state: &Path, project: &str, dispatch: &BTreeMap<String, Strength>) -> Policy {
    read_reporting(state, project, dispatch, |error| {
        warn(
            Warning::new(
                CONFIG_UNAVAILABLE,
                format!("policy settings unavailable ({error}); using compiled defaults"),
                provenant_records::warning::detected_source(CONFIG_UNAVAILABLE),
            )
            .subject("policy settings")
            .repair("correct the host policy configuration")
            .once_per_change(),
        );
    })
}

/// As [`read`], but hands an unreadable-settings error to `report` so a caller
/// with its own output contract decides how to surface it.
pub fn read_reporting(
    state: &Path,
    project: &str,
    dispatch: &BTreeMap<String, Strength>,
    report: impl FnOnce(&dyn std::fmt::Display),
) -> Policy {
    let _ = crate::read_domain::<Settings>(state, "policy").unwrap_or_else(|error| {
        report(&error);
        Settings::default()
    });
    let checkout = project_checkout(state, project);
    let mut policy = Policy::default();
    for source in sources(state, project, checkout.as_deref()) {
        policy.overlay(
            &BTreeMap::from([(source.key, source.strength)]),
            source.layer,
        );
    }
    policy.overlay(dispatch, Layer::Route);
    policy
}

/// Apply an explicit checks flag retained with the sealed profile. Policy
/// values in the dispatch map stay at `Layer::Route`; only this distinct field
/// can create the launch-layer `policy.default` candidate.
pub fn apply_profile_checks_mode(
    policy: &mut Policy,
    profile: &provenant_records::run::HarnessProfile,
) {
    if let Some(mode) = profile
        .checks_mode
        .as_deref()
        .and_then(provenant_records::policy::ChecksMode::from_checks)
    {
        policy.apply_checks(mode);
    }
}

/// Dispatch overrides are already parsed and sealed by `dispatch_profile`.
pub fn dispatch(profile: &provenant_records::run::HarnessProfile) -> BTreeMap<String, Strength> {
    let mut values = profile
        .overrides
        .iter()
        .filter_map(|entry| {
            let (name, _) = entry.split_once('=')?;
            let code = name.strip_prefix("policy.")?;
            let strength = serde_json::from_value(profile.knobs.get(name)?.clone()).ok()?;
            Some((code.to_owned(), strength))
        })
        .collect::<BTreeMap<_, _>>();
    values.extend(profile.policy_overrides.clone());
    values
}
pub fn for_task(state: &Path, project: &str, task: &provenant_records::run::TaskRequest) -> Policy {
    let project_id = task
        .profile
        .policy_project
        .map(|project| project.to_string());
    for_profile(
        state,
        project_id.as_deref().unwrap_or(project),
        &task.profile,
    )
}

/// Resolve a sealed profile's policy, retaining launch provenance for its
/// explicit checks mode.
pub fn for_profile(
    state: &Path,
    project: &str,
    profile: &provenant_records::run::HarnessProfile,
) -> Policy {
    let values = dispatch(profile);
    let mut policy = read(state, project, &values);
    apply_profile_checks_mode(&mut policy, profile);
    policy
}

pub fn overlay_knobs(
    policy: &mut Policy,
    knobs: &BTreeMap<String, provenant_records::Sourced<serde_json::Value>>,
) {
    let values = knobs
        .iter()
        .filter_map(|(name, knob)| {
            if knob.source.layer != provenant_records::Layer::Host {
                return None;
            }
            Some((
                name.strip_prefix("policy.")?.to_owned(),
                serde_json::from_value(knob.value.clone()).ok()?,
            ))
        })
        .collect();
    policy.overlay(&values, Layer::Host);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_dispatch_source(policy: &Policy) {
        let source = &policy.rule("WRITE_SCOPE").source;
        assert_eq!(source.layer, Layer::Route);
        assert_eq!(
            source.origin.as_deref(),
            Some("dispatch#policy.WRITE_SCOPE")
        );
    }

    #[test]
    fn dispatch_policy_keeps_route_provenance_in_each_resolver() {
        let dispatch = BTreeMap::from([("WRITE_SCOPE".into(), Strength::Refuse)]);
        let settings = Settings::default();
        assert_dispatch_source(&settings.resolve("project", &dispatch));
        assert_dispatch_source(&settings.resolve_with_knobs(
            "project",
            &BTreeMap::new(),
            &dispatch,
        ));

        let state = tempfile::tempdir().unwrap();
        assert_dispatch_source(&read_reporting(state.path(), "project", &dispatch, |_| {}));

        let sourced_dispatch = provenant_records::run::HarnessProfile {
            knobs: BTreeMap::from([("policy.WRITE_SCOPE".into(), serde_json::json!("refuse"))]),
            overrides: vec!["policy.WRITE_SCOPE=refuse".into()],
            knob_sources: BTreeMap::from([("policy.WRITE_SCOPE".into(), Layer::Launch)]),
            ..Default::default()
        };
        assert_dispatch_source(&for_profile(state.path(), "project", &sourced_dispatch));
    }

    #[test]
    fn explicit_strict_policy_has_launch_provenance_and_outranks_dispatch() {
        let route_dispatch = BTreeMap::from([("default".into(), Strength::Warn)]);
        let profile = provenant_records::run::HarnessProfile {
            knobs: BTreeMap::from([("policy.default".into(), serde_json::json!("refuse"))]),
            overrides: vec!["policy.default=refuse".into()],
            knob_sources: BTreeMap::from([("policy.default".into(), Layer::Launch)]),
            checks_mode: Some("strict".into()),
            ..Default::default()
        };
        let mut policy = Settings::default().resolve("project", &route_dispatch);
        apply_profile_checks_mode(&mut policy, &profile);

        let (key, strict) =
            provenant_records::policy::resolve_setting("LANE_DEPTH", None, &policy.settings);
        assert_eq!(key, "policy.default");
        assert_eq!(strict.value, Strength::Refuse);
        assert_eq!(strict.source.layer, Layer::Launch);
        assert_eq!(
            strict.source.origin.as_deref(),
            Some("--checks/--strict#policy.default")
        );
    }

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
