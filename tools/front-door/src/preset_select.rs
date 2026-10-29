use provenant_routing::{Select, Selection};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PresetChoice {
    pub preset: String,
    pub parameters: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RuleOverrides {
    default: Option<PresetChoice>,
    by_risk: Option<BTreeMap<String, PresetChoice>>,
    by_kind: Option<BTreeMap<String, PresetChoice>>,
    by_paths: Option<BTreeMap<String, PresetChoice>>,
    by_size: Option<BTreeMap<String, PresetChoice>>,
    by_readiness: Option<BTreeMap<String, PresetChoice>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesFile {
    select: RuleOverrides,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SelectionContext {
    pub risk: Option<String>,
    pub kind: Option<String>,
    pub paths: Vec<String>,
    pub size: Option<String>,
    pub ready: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PresetSelection {
    pub choice: PresetChoice,
    pub source: PresetSelectionSource,
    pub matched: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "reserved sources keep rules extensible for later triage and experiments"
)]
pub(crate) enum PresetSelectionSource {
    Default,
    Rule,
    Readiness,
    Override,
    Triage,
    Experiment,
}
impl PresetSelectionSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Rule => "rule",
            Self::Readiness => "readiness",
            Self::Override => "override",
            Self::Triage => "triage",
            Self::Experiment => "experiment",
        }
    }
}

pub(crate) fn select(
    repo_file: Option<&std::path::Path>,
    context: &SelectionContext,
    override_name: Option<&str>,
) -> Result<PresetSelection, String> {
    let mut rules = bundled()?;
    if let Some(path) = repo_file {
        let mut file = match provenant_os::fs::FileHandle::open_source(path) {
            Ok(file) => Some(file),
            Err(provenant_os::fs::FsError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                None
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        if let Some(file) = &mut file {
            let bytes = file
                .read_bounded(65_536)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let override_file: RulesFile = toml_edit::de::from_str(text)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            merge(&mut rules, override_file.select);
        }
    }
    let forced = override_name.map(|name| PresetChoice {
        preset: name.to_owned(),
        parameters: BTreeMap::new(),
    });
    let result: Selection<PresetChoice> = rules.select(
        forced.as_ref(),
        context.risk.as_deref(),
        context.kind.as_deref(),
        &context.paths,
        context.size.as_deref(),
        context.ready,
    );
    Ok(PresetSelection {
        choice: result.value,
        source: if result.source == "readiness" {
            PresetSelectionSource::Readiness
        } else if result.source == "override" {
            PresetSelectionSource::Override
        } else if result.source == "default" {
            PresetSelectionSource::Default
        } else {
            PresetSelectionSource::Rule
        },
        matched: result.matched,
    })
}

fn bundled() -> Result<Select<PresetChoice>, String> {
    let file: RulesFile = toml_edit::de::from_str(include_str!("../assets/preset-rules.toml"))
        .map_err(|error| format!("invalid bundled preset rules: {error}"))?;
    let mut rules = Select::default();
    merge(&mut rules, file.select);
    Ok(rules)
}

fn merge(rules: &mut Select<PresetChoice>, overrides: RuleOverrides) {
    if let Some(value) = overrides.default {
        rules.default = value;
    }
    if let Some(values) = overrides.by_risk {
        rules.by_risk.extend(values);
    }
    if let Some(values) = overrides.by_kind {
        rules.by_kind.extend(values);
    }
    if let Some(values) = overrides.by_paths {
        rules.by_paths.extend(values);
    }
    if let Some(values) = overrides.by_size {
        rules.by_size.extend(values);
    }
    if let Some(values) = overrides.by_readiness {
        rules.by_readiness.extend(values);
    }
}

#[cfg(test)]
mod tests {
    use super::{PresetSelectionSource, SelectionContext, select};

    #[test]
    fn rules_select_by_precedence_readiness_and_override_source() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("preset-rules.toml");
        std::fs::write(
            &file,
            r#"
[select.by_risk]
critical = { preset = "review-council" }
[select.by_kind]
docs = { preset = "chain" }
[select.by_paths]
"src/**" = { preset = "fan-out" }
[select.by_size]
large = { preset = "big-batch" }
"#,
        )
        .unwrap();
        let context = SelectionContext {
            risk: Some("critical".into()),
            kind: Some("docs".into()),
            paths: vec!["src/lib.rs".into()],
            size: Some("large".into()),
            ready: Some(true),
        };
        let path = select(Some(&file), &context, None).unwrap();
        assert_eq!(path.choice.preset, "fan-out");
        assert_eq!(path.source, PresetSelectionSource::Rule);
        assert_eq!(path.matched.as_deref(), Some("by_paths:src/**"));

        let underspecified = select(
            Some(&file),
            &SelectionContext {
                ready: Some(false),
                ..context.clone()
            },
            None,
        )
        .unwrap();
        assert_eq!(underspecified.choice.preset, "scope-then-run");
        assert_eq!(underspecified.source, PresetSelectionSource::Readiness);
        assert_eq!(
            underspecified.matched.as_deref(),
            Some("readiness:underspecified")
        );

        let override_selection = select(Some(&file), &context, Some("single")).unwrap();
        assert_eq!(override_selection.choice.preset, "single");
        assert_eq!(override_selection.source, PresetSelectionSource::Override);
    }

    #[test]
    fn repo_rules_extend_bundled_defaults_and_size_rule() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("preset-rules.toml");
        std::fs::write(&file, "[select.by_size]\nmedium = { preset = \"chain\" }\n").unwrap();
        let selected = select(
            Some(&file),
            &SelectionContext {
                size: Some("medium".into()),
                ready: Some(true),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(selected.choice.preset, "chain");
        assert_eq!(selected.source, PresetSelectionSource::Rule);
        let bundled = select(None, &SelectionContext::default(), None).unwrap();
        assert_eq!(bundled.choice.preset, "single");
        assert_eq!(bundled.source, PresetSelectionSource::Default);
    }
}
