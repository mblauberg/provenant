//! Layered launch policy for what a lane reads and where a writer starts:
//! `lanes.read_source` and `lanes.writer_base` resolved through the host,
//! host-project, repository and launch layers, and their reported shapes.
use super::interface::warn_operator;
use super::subject::{WRITER_BASE_KNOB, WriterBaseChoice, WriterBaseSetting};
use provenant_fabric_runtime::graph::Layer;
use serde_json::{Value, json};
use std::path::Path;

/// The `--source` flag is the launch layer of `lanes.read_source`. A host or
/// project knob supplies the value and the fetch-first flag; the resolver
/// starts from the `live` default and `auto` keeps the advisory pin of a
/// checkout behind its fetched upstream.
pub(super) fn read_source_choice(
    input: &Value,
    config: &provenant_config::ResolvedConfig<provenant_cli::domains::Domains>,
    state: &Path,
    project: &Path,
) -> provenant_cli::run::ReadSourceChoice {
    use provenant_cli::run::{ReadSource, ReadSourceChoice};
    use provenant_records::Layer;

    // A value no reader accepts warns and keeps auto; the retired
    // `checkout` spelling names its rename instead.
    let parse = |value: &str, origin: &str| match ReadSource::parse(value) {
        Ok(source) => source,
        Err(_) => {
            let (code, message) = if value == "checkout" {
                (
                    provenant_records::warning::READ_SOURCE_VALUE_RENAMED,
                    format!(
                        "{}=checkout is now live (source: {origin}); using auto",
                        ReadSourceChoice::KNOB
                    ),
                )
            } else {
                (
                    provenant_records::warning::LAUNCH_ADVISORY,
                    format!(
                        "{}={value:?} is not auto, live, default_branch or a ref (source: {origin}); using auto",
                        ReadSourceChoice::KNOB
                    ),
                )
            };
            provenant_records::warning::warn(provenant_records::warning::Warning::new(
                code,
                message,
                provenant_records::warning::detected_source(code),
            ));
            ReadSource::Auto
        }
    };
    let mut choice = ReadSourceChoice::default();
    if let Some(knob) = config.knobs.get(ReadSourceChoice::KNOB) {
        let value = knob.value.as_str().unwrap_or_default();
        let origin = format!("config.toml#{}", ReadSourceChoice::KNOB);
        choice.source = parse(value, &origin);
        choice.layer = knob.source.layer;
        choice.origin = Some(origin);
    }
    let mut layers = Vec::new();
    if let Some(project_dir) = provenant_config::host_project_dir(state, project) {
        layers.push((Layer::Host, project_dir.join("config.toml")));
    }
    layers.push((Layer::Project, project.join(".provenant/config.toml")));
    for (layer, path) in layers {
        let Some(value) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| provenant_config::read_source_value(&text))
        else {
            continue;
        };
        let origin = path.display().to_string();
        choice.layer = layer;
        choice.source = parse(&value, &origin);
        choice.origin = Some(origin);
    }
    if let Some(flag) = config.knobs.get("lanes.read_source_fetch_first") {
        choice.fetch_first = flag.value.as_bool().unwrap_or(false);
    }
    if let Some(source) = input.get("source").and_then(Value::as_str)
        && let Ok(source) = ReadSource::parse(source)
    {
        choice.source = source;
        choice.layer = Layer::Launch;
        choice.origin = Some("--source".into());
    }
    choice
}

/// One shape for the commit a read-only launch reads, in both `run --explain`
/// (its status) and the launch result. The source names the layer that chose
/// the ref; `commit` is null when the choice keeps the local checkout.
pub(super) fn read_source_report(
    selected: Option<&provenant_cli::run::ReadSourcePin>,
    choice: &provenant_cli::run::ReadSourceChoice,
) -> Value {
    json!({
        "value": choice.source.to_string(),
        "fetch_first": choice.fetch_first,
        "source": choice.layer.as_str(),
        "origin": choice.origin,
        "commit": selected.map(|selected| selected.commit.to_string()),
        "reference": selected.map(|selected| selected.reference.clone()),
    })
}

/// Read the `lanes.writer_base` knob from one TOML layer, accepting both the
/// flat dotted key and a `[knobs.lanes]` table, as the read-source reader does.
pub(super) fn writer_base_value(text: &str) -> Option<String> {
    let doc = text.parse::<toml_edit::DocumentMut>().ok()?;
    let knobs = doc.get("knobs")?.as_table_like()?;
    let item = knobs
        .get(WRITER_BASE_KNOB)
        .or_else(|| knobs.get("lanes")?.as_table_like()?.get("writer_base"))?;
    item.as_str().map(str::to_owned)
}

/// Resolve `lanes.writer_base` through the host, host-project, repository and
/// launch layers. `--base` is the launch layer and always wins; a bad value
/// warns and falls back to the compiled `default_branch`.
pub(super) fn writer_base_choice(
    input: &Value,
    config: &provenant_config::ResolvedConfig<provenant_cli::domains::Domains>,
    state: &Path,
    project: &Path,
) -> WriterBaseChoice {
    let mut choice = WriterBaseChoice::default();
    if let Some(knob) = config.knobs.get(WRITER_BASE_KNOB) {
        let value = knob.value.as_str().unwrap_or_default();
        match WriterBaseSetting::parse(value) {
            Ok(setting) => {
                choice.value = setting;
                choice.layer = knob.source.layer;
                choice.origin = Some(format!("config.toml#{WRITER_BASE_KNOB}"));
            }
            Err(_) => warn_operator(format!(
                "{WRITER_BASE_KNOB}={value:?} is not a valid writer base; using default_branch"
            )),
        }
    }
    let mut layers = Vec::new();
    if let Some(project_dir) = provenant_config::host_project_dir(state, project) {
        layers.push((Layer::Host, project_dir.join("config.toml")));
    }
    layers.push((Layer::Project, project.join(".provenant/config.toml")));
    for (layer, path) in layers {
        let Some(value) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| writer_base_value(&text))
        else {
            continue;
        };
        match WriterBaseSetting::parse(&value) {
            Ok(setting) => {
                choice.value = setting;
                choice.layer = layer;
                choice.origin = Some(path.display().to_string());
            }
            Err(_) => {
                choice.value = WriterBaseSetting::DefaultBranch;
                choice.layer = layer;
                choice.origin = Some(path.display().to_string());
                warn_operator(format!(
                    "{WRITER_BASE_KNOB}={value:?} (source: {}) is not a valid writer base; using default_branch",
                    path.display()
                ));
            }
        }
    }
    if let Some(base) = input
        .get("base")
        .and_then(Value::as_str)
        .filter(|base| !base.is_empty())
    {
        choice.value = WriterBaseSetting::Ref(base.to_owned());
        choice.layer = Layer::Launch;
        choice.origin = Some("--base".into());
    }
    choice
}

/// The writer base reported in `run --explain` and the launch result. `source`
/// names both the effective value and the layer that chose it; `commit` is null
/// when the writer keeps the caller checkout.
/// The writer base and what chose it: the `lanes.writer_base` value with its
/// layer, or the source that resolved a reused worktree's base when that base
/// became the launch's writer base.
pub(super) fn writer_base_report(
    base: Option<&provenant_core::GitOid>,
    choice: &WriterBaseChoice,
    resolved_by: Option<&str>,
) -> Value {
    json!({
        "commit": base.map(|base| base.to_string()),
        "source": resolved_by.map_or_else(
            || format!("{} ({})", choice.value, choice.description()),
            str::to_owned,
        ),
    })
}
