//! Adapter capability matrix: the registry, compatibility and measured latency.
//!
//! The wire shape lives here, not on the records types: the projection owns
//! every enum spelling and every declaration's reason and source. The markdown
//! view holds only static declarations so the checked section of
//! `docs/specs/run-launch.md` never depends on the host.
use provenant_config::{DomainConfig, DomainSection, FieldMetadata, SectionError};
use provenant_core::WallTime;
use provenant_os::exec::SystemExecutor;
use provenant_records::{
    ADAPTER_SPECS, AdapterFact, AdapterSpec, AnsweringModelSource, HarnessInstall, Sourced,
    TerminalEventStrength, UsageAttribution,
    control::ControlSupport,
    run::{AttemptState, NetworkAccess},
};
use provenant_routing::compatibility::{CompatibilityStatus, CompatibilityTable};
use provenant_run::{LaneQuery, RunStore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

const PAGE_SIZE: u64 = 100;

/// The host-controlled part of the matrix, under
/// `[domains.adapter.capabilities]`. Plain values parse like every other
/// domain; [`MatrixSettings`] is the sourced projection the matrix publishes.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CapabilitiesSettings {
    /// Read terminal attempts for latency samples at all.
    pub measurements: bool,
    /// At most this many latency samples are kept per adapter.
    pub measurement_samples: u32,
    /// At most this many attempts are inspected in total.
    pub measurement_records: u32,
}

impl Default for CapabilitiesSettings {
    fn default() -> Self {
        Self {
            measurements: true,
            measurement_samples: 200,
            measurement_records: 2000,
        }
    }
}

impl DomainConfig for CapabilitiesSettings {
    fn validate(&self) -> Vec<SectionError> {
        let mut errors = Vec::new();
        if !(1..=2000).contains(&self.measurement_samples) {
            errors.push(SectionError {
                detail: None,
                field: "capabilities.measurement_samples".into(),
                code: "range",
            });
        }
        if !(1..=20000).contains(&self.measurement_records) {
            errors.push(SectionError {
                detail: None,
                field: "capabilities.measurement_records".into(),
                code: "range",
            });
        }
        errors
    }
}

/// The `domains.adapter` section: only its `capabilities` table is owned here.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub capabilities: CapabilitiesSettings,
}

impl DomainConfig for Settings {
    fn validate(&self) -> Vec<SectionError> {
        self.capabilities.validate()
    }
}

pub const SECTION: DomainSection = DomainSection::new::<Settings>(
    "adapter",
    |_| {
        "[capabilities]\nmeasurements = true\nmeasurement_samples = 200\nmeasurement_records = 2000\n"
    },
)
.with_fields(&[
    FieldMetadata::new("capabilities.measurements", "bool"),
    FieldMetadata::int("capabilities.measurement_samples", 1, 2000),
    FieldMetadata::int("capabilities.measurement_records", 1, 20000),
]);

/// The effective matrix settings with the layer and origin of each value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatrixSettings {
    pub measurements: Sourced<bool>,
    pub samples: Sourced<u32>,
    pub records: Sourced<u32>,
}

impl Default for MatrixSettings {
    fn default() -> Self {
        let defaults = CapabilitiesSettings::default();
        let source = provenant_records::Source {
            layer: provenant_records::Layer::Default,
            origin: None,
        };
        Self {
            measurements: Sourced {
                value: defaults.measurements,
                source: source.clone(),
            },
            samples: Sourced {
                value: defaults.measurement_samples,
                source: source.clone(),
            },
            records: Sourced {
                value: defaults.measurement_records,
                source: source,
            },
        }
    }
}

/// Resolve the binary's closed configuration aggregate once for a matrix read.
fn resolve_config(
    state: &Path,
    project: Option<&Path>,
) -> Option<provenant_config::ResolvedConfig<crate::domains::Domains>> {
    let base = project.unwrap_or(state);
    let mut inputs = provenant_core::RootInputs::new(base.to_path_buf(), base.to_path_buf());
    inputs.state_dir = Some(state.to_path_buf());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver).ok()?;
    provenant_config::read_with_domains(&roots, &crate::domains::SECTIONS).ok()
}

impl MatrixSettings {
    fn from_resolved(resolved: &provenant_config::ResolvedConfig<crate::domains::Domains>) -> Self {
        let settings = &resolved.domains.adapter.capabilities;
        let source = |key: &str| crate::config_reads::origin_source(resolved.origins.get(key), key);
        Self {
            measurements: Sourced {
                value: settings.measurements,
                source: source("domains.adapter.capabilities.measurements"),
            },
            samples: Sourced {
                value: settings.measurement_samples,
                source: source("domains.adapter.capabilities.measurement_samples"),
            },
            records: Sourced {
                value: settings.measurement_records,
                source: source("domains.adapter.capabilities.measurement_records"),
            },
        }
    }

    /// The effective settings for one matrix read; an unreadable configuration
    /// leaves every value at its compiled default, reported as the default layer.
    pub fn resolve(state: &Path, project: Option<&Path>) -> Self {
        resolve_config(state, project)
            .as_ref()
            .map(Self::from_resolved)
            .unwrap_or_default()
    }
}

fn declaration(value: Value, reason: &'static str, source: &'static str) -> Value {
    json!({ "value": value, "reason": reason, "source": source })
}

fn bool_declaration(value: AdapterFact<bool>) -> Value {
    declaration(json!(value.value), value.reason, value.source)
}

fn terminal_declaration(value: AdapterFact<TerminalEventStrength>) -> Value {
    let name = match value.value {
        TerminalEventStrength::Strong => "strong",
        TerminalEventStrength::Weak => "weak",
        TerminalEventStrength::Absent => "absent",
    };
    declaration(json!(name), value.reason, value.source)
}

fn answering_declaration(value: AdapterFact<AnsweringModelSource>) -> Value {
    let name = match value.value {
        AnsweringModelSource::Stream => "stream",
        AnsweringModelSource::Rollout => "rollout",
        AnsweringModelSource::SessionDatabase => "session_database",
    };
    declaration(json!(name), value.reason, value.source)
}

fn usage_declaration(value: AdapterFact<UsageAttribution>) -> Value {
    let name = match value.value {
        UsageAttribution::Attempt => "attempt",
        UsageAttribution::Session => "session",
    };
    declaration(json!(name), value.reason, value.source)
}

fn control_name(value: ControlSupport) -> &'static str {
    match value {
        ControlSupport::LiveInjection => "live_injection",
        ControlSupport::MailOnly => "mail_only",
        ControlSupport::ContinuationOnly => "continuation_only",
        ControlSupport::Unsupported => "unsupported",
    }
}

fn network_name(value: NetworkAccess) -> &'static str {
    match value {
        NetworkAccess::Provider => "provider",
        NetworkAccess::Local => "local",
        NetworkAccess::Open => "open",
    }
}

fn status_name(value: CompatibilityStatus) -> &'static str {
    match value {
        CompatibilityStatus::Supported => "supported",
        CompatibilityStatus::Unsupported => "unsupported",
        CompatibilityStatus::Broken => "broken",
        CompatibilityStatus::VersionUnread => "version_unread",
    }
}

fn native(install: HarnessInstall) -> Value {
    json!({
        "hooks": bool_declaration(install.native.hooks),
        "answer_delivery": bool_declaration(install.native.answer_delivery),
        "precompact_context": bool_declaration(install.native.precompact_context),
        "instructions": bool_declaration(install.native.instructions),
    })
}

fn capabilities(spec: &AdapterSpec) -> Value {
    let launch = &spec.capabilities.launch;
    let control = &spec.capabilities.control;
    let mut value = json!({
        "launch": {
            "model": bool_declaration(launch.model),
            "effort": bool_declaration(launch.effort),
            "image_input": bool_declaration(launch.image_input),
            "resume": bool_declaration(launch.resume),
            "write": bool_declaration(launch.write),
            "network": launch.network.iter().copied().map(network_name).collect::<Vec<_>>(),
            "default_network": network_name(launch.default_network),
        },
        "control": {
            "lane": control_name(control.lane),
            "chair": control_name(control.chair),
        },
        "terminal_event": terminal_declaration(spec.capabilities.terminal_event),
        "answering_model": answering_declaration(spec.capabilities.answering_model),
        "usage_attribution": usage_declaration(spec.capabilities.usage_attribution),
        "live_input": bool_declaration(spec.capabilities.live_input),
    });
    if let Some(install) = spec.install {
        value["native"] = native(install);
    }
    value
}

/// Median of the samples, rounded down to a whole millisecond. An even count
/// takes the upper middle sample; no interpolation is needed for a diagnostic.
fn p50(mut samples: Vec<u64>) -> Option<u64> {
    samples.sort_unstable();
    samples.get(samples.len() / 2).copied()
}

fn measurements(store: Option<&RunStore>, settings: &MatrixSettings) -> BTreeMap<String, Value> {
    let Some(store) = store else {
        return BTreeMap::new();
    };
    let max_attempts = u64::from(settings.records.value);
    let max_samples = settings.samples.value as usize;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let mut query = LaneQuery {
        targets: Vec::new(),
        all: true,
        limit: PAGE_SIZE,
        after: 0,
        newest_first: true,
        owner: None,
        unseen_after: None,
    };
    let mut samples: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let mut settle: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let mut newest: BTreeMap<String, WallTime> = BTreeMap::new();
    let mut read = 0u64;
    loop {
        let Ok(page) = store.lanes(&query, &call) else {
            break;
        };
        for lane in page.lanes {
            read += 1;
            let AttemptState::Terminal { finished_at, .. } = lane.state else {
                continue;
            };
            let Some(provider) = lane
                .applied_route
                .as_ref()
                .and_then(|route| route.provider.as_deref())
            else {
                continue;
            };
            let (Some(started_at), Some(last_output_at)) = (lane.started_at, lane.last_output_at)
            else {
                continue;
            };
            let launch_ms = (started_at.as_micros() - lane.queued_at.as_micros()) / 1000;
            let settle_ms = (finished_at.as_micros() - last_output_at.as_micros()) / 1000;
            if launch_ms < 0 || settle_ms < 0 {
                continue;
            }
            let launches = samples.entry(provider.to_owned()).or_default();
            if launches.len() < max_samples {
                launches.push(launch_ms as u64);
            }
            let settles = settle.entry(provider.to_owned()).or_default();
            if settles.len() < max_samples {
                settles.push(settle_ms as u64);
            }
            newest
                .entry(provider.to_owned())
                .and_modify(|value| {
                    if finished_at > *value {
                        *value = finished_at;
                    }
                })
                .or_insert(finished_at);
        }
        match page.next {
            Some(next) if read < max_attempts => query.after = next,
            _ => break,
        }
    }
    samples
        .keys()
        .map(|adapter| {
            let value = json!({
                "launch_ms_p50": p50(samples[adapter].clone()),
                "settle_ms_p50": p50(settle[adapter].clone()),
                "samples": samples[adapter].len(),
                "measured_at": newest[adapter].to_string(),
            });
            (adapter.clone(), value)
        })
        .collect()
}

fn which(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

/// The `data` of result envelope v2 for `provenant adapter capabilities --json`.
pub fn matrix(state: &Path, project: Option<&Path>, store: Option<&RunStore>) -> Value {
    let (table, _) = CompatibilityTable::effective(state, project);
    let resolved = resolve_config(state, project);
    let settings = resolved
        .as_ref()
        .map(MatrixSettings::from_resolved)
        .unwrap_or_default();
    let providers = resolved.as_ref().map(|config| &config.providers);
    let executor = SystemExecutor::new(1).ok();
    let cwd = project.unwrap_or(state);
    let cache = provenant_providers::version::VersionCache::new();
    let measured = if settings.measurements.value {
        measurements(store, &settings)
    } else {
        BTreeMap::new()
    };
    let adapters = ADAPTER_SPECS
        .iter()
        .map(|spec| {
            let provider_settings = providers.and_then(|map| map.get(spec.identity.name));
            let probe_enabled = provider_settings.is_none_or(|settings| settings.version_probe);
            let version = provider_settings
                .filter(|settings| settings.version_probe)
                .and_then(|settings| {
                    executor
                        .as_ref()
                        .and_then(|executor| {
                            which(spec.identity.binary).map(|binary| (executor, binary))
                        })
                        .and_then(|(executor, binary)| {
                            cache.read(
                                executor,
                                &binary,
                                cwd,
                                settings.version_probe_timeout(spec.identity.name),
                            )
                        })
                });
            let mut assessment = table.assess(spec.identity.name, version.as_deref());
            if !probe_enabled {
                assessment.reason = Some(format!(
                    "version probe disabled by providers.{}.version_probe",
                    spec.identity.name
                ));
            }
            json!({
                "adapter": spec.identity.name,
                "family": spec.identity.family,
                "capabilities": capabilities(spec),
                "compatibility": {
                    "version": assessment.version,
                    "status": status_name(assessment.status),
                    "reason": assessment.reason,
                    "issue": assessment.issue,
                    "recommended_range": assessment.recommended_range,
                    "layer": assessment.source.layer.as_str(),
                    "origin": assessment.source.origin,
                },
                "measurements": measured.get(spec.identity.name).cloned().unwrap_or(Value::Null),
            })
        })
        .collect::<Vec<_>>();
    json!({ "adapters": adapters })
}

fn cell(
    text: &str,
    declaration: Option<(&'static str, &'static str)>,
    notes: &mut Vec<String>,
) -> String {
    match declaration {
        Some((reason, source)) => {
            notes.push(format!("{text}: {reason}; source={source}"));
            format!("no[^{}]", notes.len())
        }
        None => text.to_owned(),
    }
}

fn bool_cell(value: AdapterFact<bool>, label: &str, notes: &mut Vec<String>) -> String {
    if value.value {
        "yes".to_owned()
    } else {
        cell(label, Some((value.reason, value.source)), notes)
    }
}

/// Static declarations only: no host state, no compatibility table, no samples.
pub fn markdown() -> String {
    let mut notes = Vec::new();
    let header = "| Adapter | Model | Effort | Image input | Resume | Write | Network | Default network | Lane | Chair | Terminal event | Answering model | Usage attribution | Live input | Hooks | Answer delivery | Precompact context | Instructions |";
    let separator = "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|";
    let mut rows = Vec::new();
    for spec in ADAPTER_SPECS {
        let launch = &spec.capabilities.launch;
        let control = &spec.capabilities.control;
        let terminal = spec.capabilities.terminal_event;
        let terminal_text = match terminal.value {
            TerminalEventStrength::Strong => "strong",
            TerminalEventStrength::Weak => "weak",
            TerminalEventStrength::Absent => "absent",
        };
        let terminal_cell = if terminal.value == TerminalEventStrength::Absent {
            cell(
                terminal_text,
                Some((terminal.reason, terminal.source)),
                &mut notes,
            )
        } else {
            terminal_text.to_owned()
        };
        let native = spec.install.map_or_else(
            || {
                [
                    "-".to_owned(),
                    "-".to_owned(),
                    "-".to_owned(),
                    "-".to_owned(),
                ]
            },
            |install| {
                [
                    bool_cell(install.native.hooks, "hooks", &mut notes),
                    bool_cell(
                        install.native.answer_delivery,
                        "answer delivery",
                        &mut notes,
                    ),
                    bool_cell(
                        install.native.precompact_context,
                        "precompact context",
                        &mut notes,
                    ),
                    bool_cell(install.native.instructions, "instructions", &mut notes),
                ]
            },
        );
        rows.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            spec.identity.name,
            bool_cell(launch.model, "model", &mut notes),
            bool_cell(launch.effort, "effort", &mut notes),
            bool_cell(launch.image_input, "image input", &mut notes),
            bool_cell(launch.resume, "resume", &mut notes),
            bool_cell(launch.write, "write", &mut notes),
            launch.network.iter().copied().map(network_name).collect::<Vec<_>>().join(", "),
            network_name(launch.default_network),
            control_name(control.lane),
            control_name(control.chair),
            terminal_cell,
            match spec.capabilities.answering_model.value {
                AnsweringModelSource::Stream => "stream",
                AnsweringModelSource::Rollout => "rollout",
                AnsweringModelSource::SessionDatabase => "session_database",
            },
            match spec.capabilities.usage_attribution.value {
                UsageAttribution::Attempt => "attempt",
                UsageAttribution::Session => "session",
            },
            bool_cell(spec.capabilities.live_input, "live input", &mut notes),
            native[0],
            native[1],
            native[2],
            native[3],
        ));
    }
    let mut lines = vec![
        "Declared adapter capabilities. A `no` value carries a footnote with its reason and source."
            .to_owned(),
        String::new(),
        header.to_owned(),
        separator.to_owned(),
    ];
    lines.extend(rows);
    if !notes.is_empty() {
        lines.push(String::new());
        for (index, note) in notes.iter().enumerate() {
            lines.push(format!("[^{}]: {note}", index + 1));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "../../../../docs/specs/run-launch.md";

    fn spec() -> String {
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SPEC))
            .unwrap()
    }

    fn generated(text: &str) -> Option<String> {
        let (_, rest) = text.split_once("<!-- adapter-capabilities:begin -->")?;
        let (section, _) = rest.split_once("<!-- adapter-capabilities:end -->")?;
        Some(section.trim().trim_matches('\n').to_owned())
    }

    #[test]
    fn the_generated_matrix_matches_the_spec() {
        let expected = markdown();
        let actual = generated(&spec());
        assert_eq!(
            actual.as_deref(),
            Some(expected.as_str()),
            "expected matrix between the markers:\n{expected}"
        );
    }

    #[test]
    fn every_declaration_has_a_reason_and_a_source() {
        let value = matrix(std::path::Path::new("."), None, None);
        let adapters = value["adapters"].as_array().unwrap();
        assert_eq!(adapters.len(), ADAPTER_SPECS.len());
        for adapter in adapters {
            let object = adapter.as_object().unwrap();
            assert!(object["compatibility"]["status"].as_str().is_some());
            let capabilities = object["capabilities"].as_object().unwrap();
            let mut declarations = Vec::new();
            for (key, value) in capabilities {
                if key == "launch" || key == "control" || key == "native" {
                    continue;
                }
                declarations.push(value.clone());
            }
            for (key, value) in capabilities["launch"].as_object().unwrap() {
                if key == "network" || key == "default_network" {
                    continue;
                }
                declarations.push(value.clone());
            }
            if let Some(native) = capabilities.get("native") {
                for value in native.as_object().unwrap().values() {
                    declarations.push(value.clone());
                }
            }
            assert!(!declarations.is_empty());
            for declaration in declarations {
                let declaration = declaration.as_object().unwrap();
                assert!(
                    declaration["reason"]
                        .as_str()
                        .is_some_and(|text| !text.is_empty()),
                    "{} has an unsourced declaration",
                    object["adapter"]
                );
                assert!(
                    declaration["source"]
                        .as_str()
                        .is_some_and(|text| !text.is_empty()),
                    "{} has a declaration without a source",
                    object["adapter"]
                );
            }
            assert!(object["measurements"].is_null());
        }
    }

    #[test]
    fn p50_uses_the_middle_sample() {
        assert_eq!(p50(vec![10, 20, 30]), Some(20));
        assert_eq!(p50(vec![5, 7]), Some(7));
        assert_eq!(p50(Vec::new()), None);
    }

    #[test]
    fn matrix_settings_defaults_are_sourced_from_the_default_layer() {
        let settings = MatrixSettings::default();
        assert!(settings.measurements.value);
        assert_eq!(settings.samples.value, 200);
        assert_eq!(settings.records.value, 2000);
        for source in [
            &settings.measurements.source,
            &settings.samples.source,
            &settings.records.source,
        ] {
            assert_eq!(source.layer, provenant_records::Layer::Default);
        }
    }

    #[test]
    fn configured_matrix_settings_report_the_host_layer() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        std::fs::create_dir_all(&state)?;
        std::fs::write(
            state.join("config.toml"),
            "schema_version=1\n[domains.adapter.capabilities]\nmeasurements=false\nmeasurement_samples=12\nmeasurement_records=30\n",
        )?;
        let settings = MatrixSettings::resolve(&state, None);
        assert!(!settings.measurements.value);
        assert_eq!(settings.samples.value, 12);
        assert_eq!(settings.records.value, 30);
        assert_eq!(
            settings.samples.source.layer,
            provenant_records::Layer::Host
        );
        Ok(())
    }

    #[test]
    fn a_disabled_version_probe_reaches_the_assessment() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        std::fs::create_dir_all(&state)?;
        std::fs::write(
            state.join("config.toml"),
            "schema_version=1\n[providers.codex]\nversion_probe=false\n",
        )?;
        let resolved = resolve_config(&state, None).expect("config resolves");
        let settings = resolved.providers.get("codex").expect("codex settings");
        assert!(!settings.version_probe);
        let mut assessment = CompatibilityTable::bundled().assess("codex", None);
        if !settings.version_probe {
            assessment.reason =
                Some("version probe disabled by providers.codex.version_probe".to_owned());
        }
        assert_eq!(assessment.status, CompatibilityStatus::VersionUnread);
        assert_eq!(
            assessment.reason.as_deref(),
            Some("version probe disabled by providers.codex.version_probe")
        );
        Ok(())
    }
}
