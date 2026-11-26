//! Adapter capability matrix: the registry, compatibility and measured latency.
//!
//! The wire shape lives here, not on the records types: the projection owns
//! every enum spelling and every declaration's reason and source. The markdown
//! view holds only static declarations so the checked section of
//! `docs/specs/run-launch.md` never depends on the host.
use provenant_config::overlays::OverlayWarning;
use provenant_config::{DomainConfig, DomainSection, FieldMetadata, SectionError};
use provenant_core::WallTime;
use provenant_os::exec::SystemExecutor;
use provenant_records::{
    ADAPTER_SPECS, AdapterFact, AdapterSpec, AnsweringModelSource, GapAction, GapPolicy, GapRule,
    HarnessInstall, LaunchRequest, SessionReference, Sourced, TerminalEventStrength,
    UsageAttribution,
    control::ControlSupport,
    run::{AttemptState, NetworkAccess},
};
use provenant_routing::compatibility::CompatibilityTable;
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
                source,
            },
        }
    }
}

impl MatrixSettings {
    /// The effective settings from the caller's resolved configuration, with
    /// the layer and origin of each value.
    pub fn from_resolved(
        resolved: &provenant_config::ResolvedConfig<crate::domains::Domains>,
    ) -> Self {
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
}

/// Per-call flags. An empty `adapters` means every row in registry order;
/// `probe = false` is `--no-probe` and leaves every version unread.
#[derive(Clone, Copy, Debug)]
pub struct MatrixArgs<'a> {
    pub adapters: &'a [String],
    pub probe: bool,
}

impl Default for MatrixArgs<'_> {
    fn default() -> Self {
        Self {
            adapters: &[],
            probe: true,
        }
    }
}

fn sourced<T: Serialize>(value: &Sourced<T>) -> Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "value".into(),
        serde_json::to_value(&value.value).unwrap_or(Value::Null),
    );
    object.insert("layer".into(), json!(value.source.layer.as_str()));
    if let Some(origin) = &value.source.origin {
        object.insert("origin".into(), json!(origin));
    }
    Value::Object(object)
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
        AnsweringModelSource::Unreported => "unreported",
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

fn session_declaration(value: AdapterFact<SessionReference>) -> Value {
    let name = match value.value {
        SessionReference::Reported => "reported",
        SessionReference::Inferred => "inferred",
        SessionReference::Absent => "absent",
    };
    declaration(json!(name), value.reason, value.source)
}

fn caveat(value: &provenant_records::Caveat) -> Value {
    json!({ "code": value.code, "note": value.note, "source": value.source })
}

/// One gap rule as the matrix and `run --explain` show it: the action, the
/// layer and setting that chose it, and the candidates it shadowed.
pub fn gap_rule(rule: &GapRule) -> Value {
    let sourced = |action: GapAction, source: &provenant_records::Source| {
        json!({
            "action": action.as_str(),
            "layer": source.layer.as_str(),
            "origin": source.origin,
        })
    };
    let mut value = sourced(rule.action, &rule.source);
    value["shadowed"] = rule
        .shadowed
        .iter()
        .map(|candidate| sourced(candidate.value, &candidate.source))
        .collect();
    value
}

/// `run --explain`: each live node's capability gaps on its pinned adapter,
/// with the setting that chose each action, under that node's dispatch policy.
/// An unpinned route is checked when it resolves, so it is reported pending;
/// a node whose dispatch profile does not resolve reports that reason instead,
/// since dispatch refuses it on the same profile.
pub fn explain_capability_gaps(
    roots: &provenant_core::Roots,
    project: &str,
    config: &provenant_config::ResolvedConfig<crate::domains::Domains>,
    nodes: &[provenant_fabric_runtime::graph::GraphNode],
) -> Vec<Value> {
    let overlay = crate::config_reads::capability_overlay(roots);
    let mut rows = Vec::new();
    for node in nodes {
        if !provenant_fabric_runtime::graph::when::holds(&node.when) {
            continue;
        }
        let spec = node.attempt.clone().unwrap_or_default();
        let profile = match config.dispatch_profile(&spec.lane_knobs()) {
            Ok(profile) => profile,
            Err(error) => {
                rows.push(json!({
                    "node": node.id,
                    "status": "profile_error",
                    "reason": error.to_string(),
                }));
                continue;
            }
        };
        let policy = provenant_config::policy::read(
            roots.state.as_path(),
            project,
            &provenant_config::policy::dispatch(&profile),
        );
        let routes: Vec<Option<&provenant_fabric_runtime::graph::GraphRoute>> = node
            .routes
            .as_ref()
            .map(|routes| routes.iter().map(Some).collect())
            .unwrap_or_else(|| vec![node.route.as_ref()]);
        for route in routes {
            let Some(adapter) = route.and_then(|route| route.adapter.as_deref()) else {
                rows.push(json!({ "node": node.id, "status": "pending_route" }));
                continue;
            };
            let request = LaunchRequest {
                model: route.and_then(|route| route.model.as_deref()),
                effort: route.and_then(|route| route.effort.as_deref()),
                image_input: !node.attachments.is_empty(),
                write: node.access == provenant_fabric_runtime::graph::AccessMode::WorktreeWrite,
                skill_location: node.require_skill_location,
                ..Default::default()
            };
            let mut row = launch_gaps(adapter, request, &overlay, &policy);
            row["node"] = json!(node.id);
            rows.push(row);
        }
    }
    rows
}

/// The effective gap rule for each degradable field of one adapter.
pub fn gap_rules(gaps: &GapPolicy) -> Value {
    json!({
        "model": gap_rule(&gaps.model),
        "effort": gap_rule(&gaps.effort),
        "image_input": gap_rule(&gaps.image_input),
    })
}

/// The capability gaps one launch would meet on `adapter` after the owner's
/// overlay, each with its action and the setting that chose it, plus the
/// adapter's effective gap rules. `run --explain` shows this so a degraded or
/// refused control is visible before anything launches.
pub fn launch_gaps(
    adapter: &str,
    request: LaunchRequest<'_>,
    overlay: &provenant_records::CapabilityOverlay,
    policy: &provenant_records::policy::Policy,
) -> Value {
    let Some(spec) = AdapterSpec::get(adapter) else {
        return json!({ "adapter": adapter, "status": "unknown_adapter" });
    };
    let spec = spec.with_overlay(overlay);
    let gap_policy = overlay.gap_policy(spec.identity.name, policy);
    let rules = gap_rules(&gap_policy);
    let check = spec.check_launch(&LaunchRequest {
        gap_policy,
        ..request
    });
    let gaps = check
        .gaps
        .iter()
        .map(|gap| {
            json!({
                "field": gap.field.name(),
                "action": match gap.action {
                    provenant_records::LaunchAction::Refuse => "refuse",
                    provenant_records::LaunchAction::Degrade => "degrade",
                },
                "warn": gap.warn,
                "reason": gap.reason,
                "source": gap.source,
                "action_layer": gap.action_source.layer.as_str(),
                "action_origin": gap.action_source.origin,
                "message": gap.to_string(),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "adapter": spec.identity.name,
        "status": if check.refusal().is_some() { "refused" } else if gaps.is_empty() { "supported" } else { "degraded" },
        "gaps": gaps,
        "on_gap": rules,
    })
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
        "session_reference": session_declaration(spec.capabilities.session_reference),
        "caveats": spec
            .capabilities
            .caveats
            .iter()
            .map(caveat)
            .collect::<Vec<_>>(),
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

/// One adapter's latency samples. Each span is independent: a record missing
/// `first_output_at` is skipped for that span only, and older rows never
/// recorded it.
#[derive(Default)]
struct SpanSamples {
    start: Vec<u64>,
    first_output: Vec<u64>,
    settle: Vec<u64>,
    newest: Option<WallTime>,
}

fn span(from: &'static str, to: &'static str, samples: &[u64]) -> Value {
    json!({
        "from": from,
        "to": to,
        "p50_ms": p50(samples.to_vec()),
        "samples": samples.len(),
    })
}

fn render_measurements(samples: &SpanSamples) -> Value {
    json!({
        "samples": samples.start.len(),
        "measured_at": samples.newest.map(|value| value.to_string()),
        "spans": {
            "start": span("queued_at", "started_at", &samples.start),
            "first_output": span("started_at", "first_output_at", &samples.first_output),
            "settle": span("last_output_at", "finished_at", &samples.settle),
        }
    })
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
    let mut per_adapter: BTreeMap<String, SpanSamples> = BTreeMap::new();
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
            let samples = per_adapter.entry(provider.to_owned()).or_default();
            let mut contributed = false;
            if let Some(started_at) = lane.started_at {
                let ms = (started_at.as_micros() - lane.queued_at.as_micros()) / 1000;
                if ms >= 0 && samples.start.len() < max_samples {
                    samples.start.push(ms as u64);
                    contributed = true;
                }
                if let Some(first_output_at) = lane.first_output_at {
                    let ms = (first_output_at.as_micros() - started_at.as_micros()) / 1000;
                    if ms >= 0 && samples.first_output.len() < max_samples {
                        samples.first_output.push(ms as u64);
                        contributed = true;
                    }
                }
            }
            if let Some(last_output_at) = lane.last_output_at {
                let ms = (finished_at.as_micros() - last_output_at.as_micros()) / 1000;
                if ms >= 0 && samples.settle.len() < max_samples {
                    samples.settle.push(ms as u64);
                    contributed = true;
                }
            }
            if contributed && samples.newest.is_none_or(|value| finished_at > value) {
                samples.newest = Some(finished_at);
            }
        }
        match page.next {
            Some(next) if read < max_attempts => query.after = next,
            _ => break,
        }
    }
    per_adapter
        .into_iter()
        .map(|(adapter, samples)| {
            if samples.start.is_empty()
                && samples.first_output.is_empty()
                && samples.settle.is_empty()
            {
                return (adapter, Value::Null);
            }
            (adapter, render_measurements(&samples))
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
/// Default options project every adapter with the version probe enabled.
/// Without a resolved configuration every setting is its compiled default.
pub fn matrix(state: &Path, project: Option<&Path>, store: Option<&RunStore>) -> Value {
    let (value, _) = matrix_with(state, project, None, store, &MatrixArgs::default());
    value
}

/// The projection plus warnings for skipped overlay entries and unknown ids.
/// `config` is the caller's resolved configuration; the command reads it once
/// through `config_reads` with the real roots, and `None` uses the compiled
/// defaults.
pub fn matrix_with(
    state: &Path,
    project: Option<&Path>,
    config: Option<&provenant_config::ResolvedConfig<crate::domains::Domains>>,
    store: Option<&RunStore>,
    args: &MatrixArgs<'_>,
) -> (Value, Vec<OverlayWarning>) {
    let (table, mut warnings) = CompatibilityTable::effective(state, project);
    let (overlay, overlay_warnings) =
        provenant_config::adapter_capabilities::effective(state, project);
    warnings.extend(overlay_warnings);
    let settings = config
        .map(MatrixSettings::from_resolved)
        .unwrap_or_default();
    let providers = config.map(|config| &config.providers);
    let project_key =
        provenant_config::ports::project_key_for(project.unwrap_or(state)).unwrap_or_default();
    let policy: provenant_records::policy::Policy =
        provenant_config::policy::read(state, &project_key, &BTreeMap::new());
    let executor = SystemExecutor::new(1).ok();
    let cwd = project.unwrap_or(state);
    let measured = if settings.measurements.value {
        measurements(store, &settings)
    } else {
        BTreeMap::new()
    };
    let mut adapters = Vec::new();
    for spec in ADAPTER_SPECS {
        if !args.adapters.is_empty()
            && !args
                .adapters
                .iter()
                .any(|id| id.as_str() == spec.identity.name)
        {
            continue;
        }
        let provider_settings = providers.and_then(|map| map.get(spec.identity.name));
        let (version, skipped) = if !args.probe {
            (None, Some("probe skipped by --no-probe".to_owned()))
        } else {
            match provenant_providers::version::configured(spec.identity.name, provider_settings) {
                Ok(timeout) => (
                    executor.as_ref().and_then(|executor| {
                        which(spec.identity.binary).and_then(|binary| {
                            provenant_providers::version::shared()
                                .read(executor, &binary, cwd, timeout)
                        })
                    }),
                    None,
                ),
                Err(reason) => (None, Some(reason)),
            }
        };
        let mut assessment = table.assess(spec.identity.name, version.as_deref());
        if skipped.is_some() {
            assessment.reason = skipped;
        }
        let effective = spec.with_overlay(&overlay);
        let overlay_block = overlay
            .entry(effective.identity.name)
            .map_or(Value::Null, |entry| {
                json!({
                    "disabled": entry.disable.iter().map(|item| item.key()).collect::<Vec<_>>(),
                    "reason": entry.reason.clone(),
                    "layer": entry.source.layer.as_str(),
                    "origin": entry.source.origin.clone(),
                })
            });
        let gaps = overlay.gap_policy(effective.identity.name, &policy);
        adapters.push(json!({
            "adapter": effective.identity.name,
            "family": effective.identity.family,
            "capabilities": capabilities(&effective),
            "overlay": overlay_block,
            "on_gap": gap_rules(&gaps),
            "compatibility": {
                "version": assessment.version,
                "status": assessment.status.as_str(),
                "reason": assessment.reason,
                "issue": assessment.issue,
                "recommended_range": assessment.recommended_range,
                "layer": assessment.source.layer.as_str(),
                "origin": assessment.source.origin,
            },
            "measurements": measured
                .get(effective.identity.name)
                .cloned()
                .unwrap_or(Value::Null),
        }));
    }
    for id in args.adapters {
        if AdapterSpec::get(id).is_none() {
            warnings.push(OverlayWarning::new(
                provenant_records::Layer::Launch,
                "adapter capabilities --adapter",
                format!("unknown adapter `{id}`"),
            ));
        }
    }
    let value = json!({
        "settings": {
            "measurements": sourced(&settings.measurements),
            "measurement_samples": sourced(&settings.samples),
            "measurement_records": sourced(&settings.records),
        },
        "adapters": adapters,
    });
    (value, warnings)
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
    let header = "| Adapter | Model | Effort | Image input | Resume | Write | Network | Default network | Lane | Chair | Terminal event | Answering model | Usage attribution | Live input | Session reference | Hooks | Answer delivery | Precompact context | Instructions |";
    let separator = "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|";
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
        let session = spec.capabilities.session_reference;
        let session_text = match session.value {
            SessionReference::Reported => "reported",
            SessionReference::Inferred => "inferred",
            SessionReference::Absent => "absent",
        };
        let session_cell = if session.value == SessionReference::Absent {
            cell(
                session_text,
                Some((session.reason, session.source)),
                &mut notes,
            )
        } else {
            session_text.to_owned()
        };
        for caveat in spec.capabilities.caveats {
            notes.push(format!(
                "{}: {}; source={}",
                caveat.code, caveat.note, caveat.source
            ));
        }
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
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
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
                AnsweringModelSource::Unreported => "unreported",
            },
            match spec.capabilities.usage_attribution.value {
                UsageAttribution::Attempt => "attempt",
                UsageAttribution::Session => "session",
            },
            bool_cell(spec.capabilities.live_input, "live input", &mut notes),
            session_cell,
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

    /// Resolve the written host config through the same reader the command
    /// uses, with distinct home, checkout and state roots.
    fn configured(
        temp: &tempfile::TempDir,
        state: &Path,
    ) -> Result<provenant_config::ResolvedConfig<crate::domains::Domains>, Box<dyn std::error::Error>>
    {
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(state.to_path_buf());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        Ok(crate::config_reads::read_for(&roots, "adapter")?)
    }

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
                if key == "launch" || key == "control" || key == "native" || key == "caveats" {
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
        let config = configured(&temp, &state)?;
        let settings = MatrixSettings::from_resolved(&config);
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
        let settings = provenant_config::ProviderSettings {
            version_probe: false,
            ..Default::default()
        };
        // The matrix, `check` and launch preparation share this decision.
        assert_eq!(
            provenant_providers::version::configured("codex", Some(&settings)),
            Err("version probe disabled by providers.codex.version_probe".to_owned())
        );
        assert_eq!(
            CompatibilityTable::bundled().assess("codex", None).status,
            provenant_routing::compatibility::CompatibilityStatus::VersionUnread
        );
        Ok(())
    }

    #[test]
    fn settings_block_reports_sourced_values() {
        let value = matrix(std::path::Path::new("."), None, None);
        let settings = value["settings"].as_object().unwrap();
        assert_eq!(settings["measurements"]["value"], json!(true));
        assert_eq!(settings["measurements"]["layer"], json!("default"));
        assert!(settings["measurements"].get("origin").is_none());
        assert_eq!(settings["measurement_samples"]["value"], json!(200));
        assert_eq!(settings["measurement_records"]["value"], json!(2000));
    }

    #[test]
    fn markdown_names_the_session_reference_and_footnotes_absent_rows() {
        let text = markdown();
        assert!(text.contains("| Session reference |"));
        // An absent session reference is a "no" cell whose footnote names it.
        assert!(text.contains("no[^"));
        assert!(text.contains("absent: raw output carries no session id"));
    }

    #[test]
    fn every_adapter_has_overlay_gap_and_session_reference() {
        let args = MatrixArgs {
            adapters: &[],
            probe: false,
        };
        let value = matrix_with(std::path::Path::new("."), None, None, None, &args).0;
        for adapter in value["adapters"].as_array().unwrap() {
            assert!(adapter["overlay"].is_null(), "{adapter}");
            assert_eq!(adapter["on_gap"]["model"]["action"], json!("warn"));
            assert_eq!(adapter["on_gap"]["model"]["layer"], json!("default"));
            assert_eq!(
                adapter["on_gap"]["model"]["origin"],
                json!("policy.LAUNCH_MODEL_UNSUPPORTED")
            );
            assert_eq!(adapter["on_gap"]["model"]["shadowed"], json!([]));
            assert_eq!(adapter["on_gap"]["effort"]["action"], json!("warn"));
            assert_eq!(adapter["on_gap"]["image_input"]["action"], json!("warn"));
            assert!(adapter["capabilities"]["caveats"].is_array());
            let session = &adapter["capabilities"]["session_reference"];
            assert!(
                session["reason"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty())
            );
            assert!(
                session["source"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty())
            );
        }
    }

    #[test]
    fn a_fixture_overlay_appears_and_turns_the_effective_fact_false()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        std::fs::create_dir_all(&state)?;
        std::fs::write(
            state.join("config.toml"),
            "schema_version=1\n[[adapter_capabilities]]\nadapter = \"codex\"\ndisable = [\"image_input\"]\nreason = \"site policy\"\n",
        )?;
        let args = MatrixArgs {
            adapters: &[],
            probe: false,
        };
        let (value, warnings) = matrix_with(&state, None, None, None, &args);
        assert!(warnings.is_empty(), "{warnings:?}");
        let adapter = value["adapters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["adapter"] == json!("codex"))
            .expect("codex row");
        assert_eq!(adapter["overlay"]["disabled"], json!(["image_input"]));
        assert_eq!(adapter["overlay"]["reason"], json!("site policy"));
        assert_eq!(adapter["overlay"]["layer"], json!("host"));
        assert_eq!(
            adapter["capabilities"]["launch"]["image_input"]["value"],
            json!(false)
        );
        assert_eq!(
            adapter["capabilities"]["launch"]["image_input"]["source"],
            json!(provenant_records::OVERLAY_SOURCE)
        );
        Ok(())
    }

    // Spec: docs/specs/run-launch.md#capability-overrides-and-gap-policy
    #[test]
    fn explain_reports_each_pinned_launch_gap_with_the_setting_that_chose_it()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{GraphNode, GraphRoute};
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        std::fs::create_dir_all(&state)?;
        let roots = |temp: &tempfile::TempDir| -> Result<provenant_core::Roots, Box<dyn std::error::Error>> {
            let mut inputs = provenant_core::RootInputs::new(
                temp.path().join("home"),
                temp.path().join("checkout"),
            );
            inputs.state_dir = Some(state.clone());
            Ok(inputs.resolve(&provenant_os::fs::SystemPathResolver)?)
        };
        let route: GraphRoute =
            serde_json::from_value(json!({"adapter": "command", "effort": "high"}))?;
        let nodes = [
            GraphNode {
                id: "pinned".into(),
                route: Some(route),
                ..Default::default()
            },
            GraphNode {
                id: "unrouted".into(),
                ..Default::default()
            },
        ];
        // No owner setting: the compiled default warns and names both settings.
        let config = configured(&temp, &state)?;
        let rows = explain_capability_gaps(&roots(&temp)?, "project", &config, &nodes);
        assert_eq!(rows.len(), 2);
        let pinned = &rows[0];
        assert_eq!(pinned["node"], json!("pinned"));
        assert_eq!(pinned["adapter"], json!("command"));
        assert_eq!(pinned["status"], json!("degraded"));
        let gap = &pinned["gaps"][0];
        assert_eq!(gap["field"], json!("effort"));
        assert_eq!(gap["action"], json!("degrade"));
        assert_eq!(gap["warn"], json!(true));
        assert_eq!(gap["action_layer"], json!("default"));
        assert_eq!(
            gap["action_origin"],
            json!("policy.LAUNCH_EFFORT_UNSUPPORTED")
        );
        assert!(
            gap["message"].as_str().is_some_and(|message| message.contains(
                "warn by default; set policy.LAUNCH_EFFORT_UNSUPPORTED or [[adapter_capabilities]] on_gap.effort"
            )),
            "{gap}"
        );
        assert_eq!(pinned["on_gap"]["effort"]["shadowed"], json!([]));
        assert_eq!(
            rows[1],
            json!({"node": "unrouted", "status": "pending_route"})
        );

        // A host adapter entry wins and records the policy value it replaced.
        std::fs::write(
            state.join("config.toml"),
            "schema_version=1\n[[adapter_capabilities]]\nadapter = \"command\"\non_gap = { effort = \"refuse\" }\n",
        )?;
        let config = configured(&temp, &state)?;
        let rows = explain_capability_gaps(&roots(&temp)?, "project", &config, &nodes);
        let pinned = &rows[0];
        assert_eq!(pinned["status"], json!("refused"));
        let gap = &pinned["gaps"][0];
        assert_eq!(gap["action"], json!("refuse"));
        assert_eq!(gap["action_layer"], json!("host"));
        assert!(
            gap["action_origin"]
                .as_str()
                .is_some_and(|origin| origin.ends_with("[adapter=command].on_gap.effort")),
            "{gap}"
        );
        let effort = &pinned["on_gap"]["effort"];
        assert_eq!(effort["action"], json!("refuse"));
        assert_eq!(
            effort["shadowed"],
            json!([{"action": "warn", "layer": "default", "origin": "policy.LAUNCH_EFFORT_UNSUPPORTED"}])
        );
        Ok(())
    }

    #[test]
    fn no_probe_reports_version_unread() {
        let args = MatrixArgs {
            adapters: &[],
            probe: false,
        };
        let value = matrix_with(std::path::Path::new("."), None, None, None, &args).0;
        for adapter in value["adapters"].as_array().unwrap() {
            assert_eq!(adapter["compatibility"]["status"], json!("version_unread"));
            assert_eq!(
                adapter["compatibility"]["reason"],
                json!("probe skipped by --no-probe")
            );
        }
    }

    #[test]
    fn measurements_off_gives_null() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        std::fs::create_dir_all(&state)?;
        std::fs::write(
            state.join("config.toml"),
            "schema_version=1\n[domains.adapter.capabilities]\nmeasurements=false\n",
        )?;
        let config = configured(&temp, &state)?;
        let args = MatrixArgs::default();
        let value = matrix_with(&state, None, Some(&config), None, &args).0;
        assert_eq!(value["settings"]["measurements"]["value"], json!(false));
        for adapter in value["adapters"].as_array().unwrap() {
            assert!(adapter["measurements"].is_null());
        }
        Ok(())
    }

    #[test]
    fn fixture_timestamps_give_three_p50s() {
        let samples = SpanSamples {
            start: vec![1000, 3000, 2000],
            first_output: vec![500, 700, 600],
            settle: vec![300, 100, 200],
            newest: None,
        };
        let value = render_measurements(&samples);
        assert_eq!(value["samples"], json!(3));
        assert_eq!(value["spans"]["start"]["from"], json!("queued_at"));
        assert_eq!(value["spans"]["start"]["to"], json!("started_at"));
        assert_eq!(value["spans"]["start"]["p50_ms"], json!(2000));
        assert_eq!(value["spans"]["first_output"]["from"], json!("started_at"));
        assert_eq!(
            value["spans"]["first_output"]["to"],
            json!("first_output_at")
        );
        assert_eq!(value["spans"]["first_output"]["p50_ms"], json!(600));
        assert_eq!(value["spans"]["settle"]["from"], json!("last_output_at"));
        assert_eq!(value["spans"]["settle"]["to"], json!("finished_at"));
        assert_eq!(value["spans"]["settle"]["p50_ms"], json!(200));
    }

    #[test]
    fn unknown_adapter_flag_warns_and_is_skipped() {
        let unknown = vec!["no-such-adapter".to_owned()];
        let args = MatrixArgs {
            adapters: &unknown,
            probe: false,
        };
        let (value, warnings) = matrix_with(std::path::Path::new("."), None, None, None, &args);
        assert!(value["adapters"].as_array().unwrap().is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].reason.contains("unknown adapter"));
    }
}
