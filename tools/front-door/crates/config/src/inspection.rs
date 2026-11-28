//! Input observations collected by existing readers, without additional I/O,
//! and setting explanations assembled from the same declared sources.
use provenant_os::fs::{FileKind, Metadata};
use provenant_records::{CanonicalV1, FileDigest};
use std::path::Path;

#[derive(Default)]
pub(crate) struct InspectionEvidence {
    observations: Vec<[u8; 32]>,
}
impl InspectionEvidence {
    /// Hash length-framed native path bytes, the observation kind, presence,
    /// and exact content bytes. Raw bytes use file_digest, not record_digest.
    pub(crate) fn observe(&mut self, kind: &[u8], path: &Path, bytes: Option<&[u8]>) {
        let mut frame = b"provenant-check-observation-v1\0".to_vec();
        for part in [kind, path.as_os_str().as_encoded_bytes()] {
            frame.extend_from_slice(&(part.len() as u64).to_be_bytes());
            frame.extend_from_slice(part);
        }
        frame.push(u8::from(bytes.is_some()));
        if let Some(bytes) = bytes {
            frame.extend_from_slice(CanonicalV1::file_digest(bytes).bytes());
        }
        self.observations
            .push(*CanonicalV1::file_digest(&frame).bytes());
    }
    pub(crate) fn metadata(&mut self, path: &Path, metadata: Metadata) {
        let mut bytes = vec![match metadata.kind {
            FileKind::Regular => 0,
            FileKind::Directory => 1,
            FileKind::Symlink => 2,
            FileKind::Other => 3,
        }];
        for value in [
            metadata.device,
            metadata.inode,
            metadata.links,
            metadata.size,
            metadata.allocated,
            u64::from(metadata.uid),
            u64::from(metadata.mode),
        ] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        // Filesystem timestamps are already i128; preserve their bytes rather
        // than passing an out-of-range JSON integer to CanonicalV1.
        bytes.extend_from_slice(&metadata.modified_ns.to_be_bytes());
        self.observe(b"metadata", path, Some(&bytes));
    }
    pub(crate) fn finish(mut self) -> FileDigest {
        self.observations.sort();
        let mut bytes = b"provenant-check-inputs-v1\0".to_vec();
        for digest in self.observations {
            bytes.extend_from_slice(&digest);
        }
        CanonicalV1::file_digest(&bytes)
    }
}

// -- setting explanations ---------------------------------------------------

use crate::ConfigError;
use provenant_core::Roots;
use provenant_records::{
    Layer, Source,
    provenance::CANONICAL_ORDER,
    warning::{self, SETTING_LAYER_IGNORED},
};
use serde::Serialize;
use std::{collections::BTreeMap, io::Read, sync::OnceLock};

/// One setting candidate printed by `config explain`: its value, the layer and
/// exact place that declared it, and how specific its key is inside that layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExplanationCandidate {
    pub key: String,
    pub value: serde_json::Value,
    pub layer: Layer,
    pub origin: Option<String>,
    pub specificity: u8,
}

/// The winning candidate, every candidate it replaced, and every candidate
/// ignored because its family does not declare that layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SettingExplanation {
    pub key: String,
    pub winner: Option<ExplanationCandidate>,
    /// Losing candidates, from least to most specific within their layer.
    pub shadowed: Vec<ExplanationCandidate>,
    pub ignored: Vec<ExplanationCandidate>,
}

impl SettingExplanation {
    /// Layer wins across families; a more specific key wins within one layer.
    /// Ties keep the last candidate given, matching the shared `Precedence`.
    fn resolve(key: &str, candidates: Vec<ExplanationCandidate>, allowed: &[Layer]) -> Self {
        let mut ranked = Vec::new();
        let mut ignored = Vec::new();
        for candidate in candidates {
            if allowed.contains(&candidate.layer) {
                ranked.push(candidate);
            } else {
                ignored.push(candidate);
            }
        }
        ranked.sort_by_key(|candidate| {
            (
                CANONICAL_ORDER
                    .iter()
                    .position(|layer| *layer == candidate.layer)
                    .unwrap_or(CANONICAL_ORDER.len()),
                candidate.specificity,
            )
        });
        let winner = ranked.pop();
        for candidate in &ignored {
            warning::warn(
                warning::Warning::new(
                    SETTING_LAYER_IGNORED,
                    format!(
                        "{key} from {} ignored: this family does not declare that layer",
                        candidate.layer.description()
                    ),
                    warning::detected_source(SETTING_LAYER_IGNORED),
                )
                .subject(key.to_owned())
                .source(Source {
                    layer: candidate.layer,
                    origin: candidate.origin.clone(),
                })
                .repair(format!("set {key} from a layer this family declares")),
            );
        }
        Self {
            key: key.to_owned(),
            winner,
            shadowed: ranked,
            ignored,
        }
    }
}

fn candidate(
    key: &str,
    value: serde_json::Value,
    layer: Layer,
    origin: &str,
    specificity: u8,
) -> ExplanationCandidate {
    ExplanationCandidate {
        key: key.to_owned(),
        value,
        layer,
        origin: Some(origin.to_owned()),
        specificity,
    }
}

/// Authority settings are honoured only from the host layer. Values at any
/// other layer are reported as ignored, never silently dropped.
fn authority(key: &str) -> bool {
    key.starts_with("domains.pr.land.")
        || key == "domains.pr.land"
        || key == "admin_publication"
        || key.ends_with(".admin_publication")
        || key.contains("ruleset")
}

const AUTHORITY_LAYERS: &[Layer] = &[Layer::Default, Layer::Host];

fn json_value(item: &toml_edit::Item) -> Option<serde_json::Value> {
    match item.as_value()? {
        toml_edit::Value::Boolean(value) => Some(serde_json::Value::Bool(*value.value())),
        toml_edit::Value::Integer(value) => Some(serde_json::Value::from(*value.value())),
        toml_edit::Value::Float(value) => {
            serde_json::Number::from_f64(*value.value()).map(serde_json::Value::Number)
        }
        toml_edit::Value::String(value) => Some(serde_json::Value::from(value.value().as_str())),
        toml_edit::Value::Array(items) => items
            .iter()
            .map(|item| json_value(&toml_edit::Item::Value(item.clone())))
            .collect::<Option<Vec<_>>>()
            .map(serde_json::Value::Array),
        toml_edit::Value::InlineTable(table) => table
            .iter()
            .map(|(key, value)| {
                Some((
                    key.to_owned(),
                    json_value(&toml_edit::Item::Value(value.clone()))?,
                ))
            })
            .collect::<Option<serde_json::Map<_, _>>>()
            .map(serde_json::Value::Object),
        _ => None,
    }
}

/// Flatten dotted keys and nested tables the same way the knob editor does, so
/// `knobs.lanes.pr = true` and `[knobs.lanes] pr = true` explain identically.
fn flatten(
    table: &dyn toml_edit::TableLike,
    prefix: &str,
    out: &mut Vec<(String, serde_json::Value)>,
) {
    for (key, item) in table.iter() {
        let name = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        match item.as_table_like() {
            Some(nested) => flatten(nested, &name, out),
            None => {
                if let Some(value) = json_value(item) {
                    out.push((name, value));
                }
            }
        }
    }
}

pub(crate) fn section_values(text: &str, section: &str) -> Vec<(String, serde_json::Value)> {
    let mut out = Vec::new();
    let Ok(document) = text.parse::<toml_edit::DocumentMut>() else {
        return out;
    };
    if let Some(table) = document
        .get(section)
        .and_then(toml_edit::Item::as_table_like)
    {
        flatten(table, "", &mut out);
    }
    out
}

fn host_text(roots: &Roots) -> Result<Option<String>, ConfigError> {
    let path = roots.state.as_path().join("config.toml");
    match crate::loader::open(roots)? {
        Some(mut file) => Ok(Some(crate::loader::bounded_text(
            &mut file,
            path.as_path(),
        )?)),
        None => Ok(None),
    }
}

/// Read an optional project file through the no-follow boundary. An unreadable
/// or absent policy file has no candidates; it never stops an explanation.
pub(crate) fn project_text(project: Option<&Path>, name: &str) -> Option<String> {
    let dir = provenant_os::fs::Directory::open_anchor(&project?.join(".provenant")).ok()?;
    let file = dir.open_read(std::ffi::OsStr::new(name)).ok()?;
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 1024 * 1024 {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[derive(serde::Deserialize)]
struct ExplainRegistry {
    #[serde(default)]
    knobs: BTreeMap<String, ExplainSpec>,
    #[serde(default)]
    presets: BTreeMap<String, ExplainPreset>,
}
#[derive(serde::Deserialize)]
struct ExplainSpec {
    default: serde_json::Value,
}
#[derive(serde::Deserialize)]
struct ExplainPreset {
    #[serde(default)]
    knobs: BTreeMap<String, serde_json::Value>,
}

fn registry() -> Result<&'static ExplainRegistry, ConfigError> {
    static PARSED: OnceLock<Option<ExplainRegistry>> = OnceLock::new();
    PARSED
        .get_or_init(|| {
            serde_json::from_str(include_str!("../../../../../config/harness-knobs.json")).ok()
        })
        .as_ref()
        .ok_or(ConfigError::KnobRegistry)
}

fn strength_name(strength: provenant_records::policy::Strength) -> &'static str {
    match strength {
        provenant_records::policy::Strength::Refuse => "refuse",
        provenant_records::policy::Strength::Warn => "warn",
        provenant_records::policy::Strength::Off => "off",
    }
}

fn policy_keys() -> Vec<String> {
    let mut keys = vec!["policy.default".to_owned()];
    for rule in provenant_records::policy::table() {
        keys.push(format!("policy.{}", rule.code));
        if let Some(group) = &rule.group {
            keys.push(format!("policy.{group}"));
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

/// Explain one declared setting key: the winning value and every candidate it
/// beat, each with its layer, origin and key specificity. Unknown keys carry
/// close matches from the one declaration registry.
pub fn explain(
    key: &str,
    roots: &Roots,
    project: Option<&Path>,
) -> Result<SettingExplanation, ConfigError> {
    let registry = registry()?;
    let resolved = crate::loader::config_show(roots, &crate::domains::NO_DOMAINS)?;
    let host = host_text(roots)?;
    let allowed: &[Layer] = if authority(key) {
        AUTHORITY_LAYERS
    } else if key.starts_with("policy.") {
        &CANONICAL_ORDER
    } else {
        crate::knobs::KNOB_PRECEDENCE.0
    };
    let mut candidates = Vec::new();

    if let Some(code) = key.strip_prefix("policy.") {
        let rule = provenant_records::policy::table()
            .iter()
            .find(|rule| rule.code == code || rule.group.as_deref() == Some(code));
        if code == "default" || rule.is_some() {
            let mut scans: Vec<(&str, u8)> = Vec::new();
            if code == "default" {
                scans.push((code, 1));
            } else if let Some(rule) = rule {
                scans.push((code, if rule.code == code { 3 } else { 2 }));
                if let Some(group) = rule.group.as_deref() {
                    if group != code {
                        scans.push((group, 2));
                        scans.push(("default", 1));
                    }
                } else {
                    scans.push(("default", 1));
                }
            }
            for (suffix, specificity) in scans {
                let full = format!("policy.{suffix}");
                if let Some(rule) = provenant_records::policy::table()
                    .iter()
                    .find(|rule| rule.code == suffix)
                {
                    candidates.push(candidate(
                        &full,
                        serde_json::Value::from(strength_name(rule.strength)),
                        Layer::Default,
                        "crates/run/assets/policy.toml (compiled default)",
                        3,
                    ));
                }
                let project_id = project
                    .and_then(crate::project_binding::project_id_for_path)
                    .map(|id| id.to_string());
                let project_id = project_id.as_deref().unwrap_or("");
                let checkout = crate::policy::project_checkout(roots.state.as_path(), project_id);
                for source in
                    crate::policy::sources(roots.state.as_path(), project_id, checkout.as_deref())
                {
                    if source.key == suffix {
                        candidates.push(candidate(
                            &full,
                            serde_json::json!(source.strength),
                            source.layer,
                            &source.origin,
                            specificity,
                        ));
                    }
                }
            }
        }
    } else if let Some(spec) = registry.knobs.get(key) {
        candidates.push(candidate(
            key,
            spec.default.clone(),
            Layer::Default,
            &format!("config/harness-knobs.json#knobs.{key}"),
            3,
        ));
        let profile = resolved.profile.as_str();
        if let Some(value) = registry
            .presets
            .get(profile)
            .and_then(|preset| preset.knobs.get(key))
        {
            candidates.push(candidate(
                key,
                value.clone(),
                Layer::Profile,
                &format!("preset:{profile}#knobs.{key}"),
                3,
            ));
        }
        if let Some(text) = &host {
            for (name, value) in section_values(text, "knobs") {
                if name == *key {
                    candidates.push(candidate(
                        key,
                        value,
                        Layer::Host,
                        &format!("config.toml#knobs.{key}"),
                        3,
                    ));
                }
            }
        }
    } else {
        // Port leaves already carry the exact candidate history of the table.
        if let Ok(table) =
            crate::ports::PortTable::read_for_inspection(project, roots.state.as_path())
        {
            for source in table.sourced_leaf(key) {
                candidates.push(ExplanationCandidate {
                    key: key.to_owned(),
                    value: source.value.clone(),
                    layer: source.source.layer,
                    origin: source.source.origin.clone(),
                    specificity: 3,
                });
            }
        }
        // Config-owned fields report one origin (compiled or the host file).
        if let Some(origin) = resolved.origins.get(key) {
            let pointer = format!("/{}", key.replace('.', "/"));
            let value = serde_json::to_value(&resolved)
                .ok()
                .and_then(|resolved| resolved.pointer(&pointer).cloned())
                .unwrap_or(serde_json::Value::Null);
            let (layer, place) = match origin {
                crate::Origin::Compiled { profile } => (
                    Layer::Default,
                    format!("compiled {} profile#{key}", profile.as_str()),
                ),
                crate::Origin::User { path } => (
                    Layer::Host,
                    format!("{}#{key}", path.as_path_text().unwrap_or("config.toml")),
                ),
            };
            candidates.push(candidate(key, value, layer, &place, 3));
        }
    }

    if candidates.is_empty() {
        let mut known = registry.knobs.keys().cloned().collect::<Vec<_>>();
        known.extend(policy_keys());
        let close = provenant_core::close_matches(key, known.iter().map(String::as_str))
            .into_iter()
            .map(str::to_owned)
            .collect();
        return Err(ConfigError::UnknownKnob {
            name: key.to_owned(),
            close,
        });
    }

    Ok(SettingExplanation::resolve(key, candidates, allowed))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod explanation_tests {
    use super::*;
    use provenant_records::warning;

    fn roots(dir: &Path) -> Roots {
        crate::tests::state_roots(dir)
    }

    fn write_config(dir: &Path, text: &str) {
        std::fs::write(dir.join("state/config.toml"), text).unwrap();
    }

    #[test]
    fn config_explain_lists_winner_and_losing_candidates() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        write_config(
            temp.path(),
            "schema_version = 1\nprofile = 'balanced'\n[knobs]\nlanes.max_concurrency = 7\n",
        );
        let explained = explain("lanes.max_concurrency", &roots, None).unwrap();
        let winner = explained.winner.expect("a declared knob always resolves");
        assert_eq!(winner.value, serde_json::json!(7));
        assert_eq!(winner.layer, Layer::Host);
        assert_eq!(
            winner.origin.as_deref(),
            Some("config.toml#knobs.lanes.max_concurrency")
        );
        let default = explained
            .shadowed
            .iter()
            .find(|candidate| candidate.layer == Layer::Default)
            .expect("the compiled default is shown as a loser");
        assert_ne!(default.value, winner.value);
        assert!(
            default
                .origin
                .as_deref()
                .is_some_and(|origin| origin.contains("harness-knobs.json"))
        );
    }

    #[test]
    fn explain_and_enforcement_share_host_and_project_policy_sources() {
        use provenant_records::policy::{Effect, Facts, Strength};
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        let checkout = temp.path().join("project");
        let git = checkout.join(".git");
        std::fs::create_dir_all(git.join("objects")).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(checkout.join(".provenant")).unwrap();
        let checkout = std::fs::canonicalize(checkout).unwrap();
        let git = std::fs::canonicalize(git).unwrap();
        let location =
            crate::project_binding::ProjectLocation::from_verified_primary_checkout(git, &checkout)
                .unwrap();
        let binding = crate::project_binding::bind_project(&roots, location).unwrap();
        let code = "PLAN_CONFINEDPLAN_NEW_AUTHORITY";
        write_config(
            temp.path(),
            "schema_version = 1\nprofile = 'balanced'\n[policy]\ndefault = 'refuse'\n",
        );
        let host = crate::policy::read(
            roots.state.as_path(),
            &binding.project_id().to_string(),
            &Default::default(),
        );
        assert!(matches!(
            host.check(code, Facts::violation("fixture")),
            Effect::Refuse(_)
        ));
        let host_explain = explain(&format!("policy.{code}"), &roots, Some(&checkout)).unwrap();
        assert_eq!(
            host_explain.winner.unwrap().value,
            serde_json::json!("refuse")
        );
        std::fs::write(
            checkout.join(".provenant/policy.toml"),
            "[policy]\ndefault = 'warn'\n",
        )
        .unwrap();
        let project = crate::policy::read(
            roots.state.as_path(),
            &binding.project_id().to_string(),
            &Default::default(),
        );
        assert!(matches!(
            project.check(code, Facts::violation("fixture")),
            Effect::Warn(_)
        ));
        let explained = explain(&format!("policy.{code}"), &roots, Some(&checkout)).unwrap();
        let winner = explained.winner.unwrap();
        assert_eq!(
            (winner.layer, winner.value),
            (Layer::Project, serde_json::json!("warn"))
        );
        let strict = crate::policy::read(
            roots.state.as_path(),
            &binding.project_id().to_string(),
            &std::collections::BTreeMap::from([("default".into(), Strength::Refuse)]),
        );
        assert!(matches!(
            strict.check(code, Facts::violation("fixture")),
            Effect::Refuse(_)
        ));
    }

    #[test]
    fn higher_layer_beats_a_more_specific_key() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        write_config(
            temp.path(),
            "schema_version = 1\nprofile = 'balanced'\n[policy]\nPR_ATTEMPT_STATE = 'refuse'\ndefault = 'warn'\n",
        );
        let explained = explain("policy.PR_ATTEMPT_STATE", &roots, None).unwrap();
        let winner = explained.winner.expect("policy rule resolves");
        assert_eq!(winner.layer, Layer::Host);
        assert_eq!(winner.value, serde_json::json!("refuse"));
        assert_eq!(winner.key, "policy.PR_ATTEMPT_STATE");
        assert_eq!(winner.specificity, 3);
        let default = explained
            .shadowed
            .iter()
            .find(|candidate| candidate.key == "policy.default")
            .expect("the less specific key is still reported");
        assert_eq!(default.specificity, 1);
        // A group key is more specific than `default` but less than the code.
        assert!(
            explained
                .shadowed
                .iter()
                .all(|candidate| candidate.specificity <= 3)
        );
    }

    #[test]
    fn authority_setting_ignores_non_host_layers() {
        assert!(authority("domains.pr.land.enabled"));
        let candidates = vec![
            candidate(
                "domains.pr.land.enabled",
                serde_json::json!(false),
                Layer::Default,
                "compiled",
                3,
            ),
            candidate(
                "domains.pr.land.enabled",
                serde_json::json!(false),
                Layer::Profile,
                "preset:economy",
                3,
            ),
            candidate(
                "domains.pr.land.enabled",
                serde_json::json!(true),
                Layer::Host,
                "config.toml",
                3,
            ),
            candidate(
                "domains.pr.land.enabled",
                serde_json::json!(true),
                Layer::Project,
                ".provenant/config.toml",
                3,
            ),
        ];
        let (explained, warnings) = warning::capture(|| {
            SettingExplanation::resolve("domains.pr.land.enabled", candidates, AUTHORITY_LAYERS)
        });
        assert!(
            explained
                .ignored
                .iter()
                .any(|candidate| candidate.layer == Layer::Profile)
        );
        assert!(
            explained
                .ignored
                .iter()
                .any(|candidate| candidate.layer == Layer::Project)
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.code == "SETTING_LAYER_IGNORED")
        );
        let winner = explained.winner.expect("host authority setting resolves");
        assert_eq!(winner.layer, Layer::Host);
        assert_eq!(winner.value, serde_json::json!(true));
    }

    #[test]
    fn unknown_key_carries_close_matches() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        let error = explain("lanes.max_concurrenc", &roots, None).unwrap_err();
        match error {
            ConfigError::UnknownKnob { name, close } => {
                assert_eq!(name, "lanes.max_concurrenc");
                assert!(
                    close
                        .iter()
                        .any(|candidate| candidate == "lanes.max_concurrency")
                );
            }
            other => panic!("expected an unknown knob, got {other:?}"),
        }
    }

    #[test]
    fn string_knob_from_host_config_is_explained() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(temp.path());
        write_config(
            temp.path(),
            "schema_version = 1\nprofile = 'balanced'\n[knobs]\nredaction = 'secrets'\n",
        );
        let explained = explain("redaction", &roots, None).unwrap();
        let winner = explained.winner.expect("a declared knob always resolves");
        assert_eq!(winner.value, serde_json::json!("secrets"));
        assert_eq!(winner.layer, Layer::Host);
        assert_eq!(
            winner.origin.as_deref(),
            Some("config.toml#knobs.redaction")
        );
    }

    #[test]
    fn project_text_reads_policy_through_the_no_follow_anchor() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(project.join(".provenant")).unwrap();
        let text = "[policy]\nPR_ATTEMPT_STATE = 'warn'\n";
        std::fs::write(project.join(".provenant/policy.toml"), text).unwrap();
        assert_eq!(
            project_text(Some(&project), "policy.toml").as_deref(),
            Some(text)
        );
        assert_eq!(project_text(Some(&project), "missing.toml"), None);
        assert_eq!(project_text(None, "policy.toml"), None);
    }
}
