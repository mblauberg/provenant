//! Inert `provenant profile` command: presentation and editing share one resolver.
use crate::{
    Result,
    codex::CommandResult,
    invalid,
    ordered::{self, Json},
    profiles::{self, Context, Registry},
};

#[derive(Debug, clap::Parser)]
#[command(name = "provenant profile")]
pub struct Arguments {
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    List {
        #[arg(long)]
        json: bool,
    },
    Show {
        #[arg(long)]
        resolved: bool,
        #[arg(long)]
        task_class: Option<String>,
        #[arg(long)]
        risk_tier: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Use {
        preset: String,
        #[arg(long)]
        project: bool,
        #[arg(long)]
        task_class: Option<String>,
    },
    Set {
        #[arg(required=true,num_args=1..)]
        assignments: Vec<String>,
        #[arg(long)]
        project: bool,
        #[arg(long)]
        task_class: Option<String>,
    },
    Reset {
        #[arg(required=true,num_args=1..)]
        knobs: Vec<String>,
        #[arg(long)]
        project: bool,
        #[arg(long)]
        task_class: Option<String>,
    },
    Diff {
        left: String,
        right: String,
        #[arg(long)]
        task_class: Option<String>,
        #[arg(long)]
        json: bool,
    },
}
impl Command {
    fn task_class(&self) -> Option<&str> {
        match self {
            Self::List { .. } => None,
            Self::Show { task_class, .. }
            | Self::Use { task_class, .. }
            | Self::Set { task_class, .. }
            | Self::Reset { task_class, .. }
            | Self::Diff { task_class, .. } => task_class.as_deref(),
        }
    }
}
fn shown(value: &Json) -> Result<String> {
    ordered::dumps(value, false, false)
}
fn target(project: bool, context: &Context<'_>) -> Result<std::path::PathBuf> {
    if project {
        profiles::project_path(context.cwd)
    } else {
        profiles::user_path(context.environment, context.homes)
    }
}
fn part(
    document: &mut Json,
    task_class: Option<&str>,
    change: impl FnOnce(&mut Json) -> Result<()>,
) -> Result<()> {
    if let Some(task) = task_class.filter(|s| !s.is_empty()) {
        let mut classes = document.get("task_classes").cloned().unwrap_or_default();
        let mut part = classes.get(task).cloned().unwrap_or_default();
        change(&mut part)?;
        classes.set(task, part)?;
        document.set("task_classes", classes)
    } else {
        change(document)
    }
}
fn unknown_preset(registry: &Registry, name: &str) -> crate::Error {
    invalid(format!(
        "unknown preset {}; known: {}",
        provenant_core::py::python_repr(name),
        registry.preset_names().collect::<Vec<_>>().join(", ")
    ))
}
pub fn run(registry: &Registry, command: &Command, context: &Context<'_>) -> CommandResult {
    match execute(registry, command, context) {
        Ok(stdout) => CommandResult {
            exit_code: 0,
            stdout,
            stderr: String::new(),
        },
        Err(e) => CommandResult {
            exit_code: 2,
            stdout: String::new(),
            stderr: format!("provenant profile: {e}\n"),
        },
    }
}
fn execute(registry: &Registry, command: &Command, context: &Context<'_>) -> Result<String> {
    let task_class = command.task_class();
    if let Some(class) = task_class.filter(|c| !c.is_empty()) {
        if !registry.task_classes.iter().any(|c| c == class) {
            return Err(invalid(format!(
                "unknown task class {}; known: {}",
                provenant_core::py::python_repr(class),
                registry.task_classes.join(", ")
            )));
        }
    }
    let mut out = String::new();
    match command {
        Command::List { json } => {
            if *json {
                return Ok(format!(
                    "{}\n",
                    ordered::dumps(
                        &Json::Object(vec![
                            ("knobs".into(), Json::Object(registry.knobs.clone())),
                            ("presets".into(), Json::Object(registry.presets.clone()))
                        ]),
                        true,
                        false
                    )?
                ));
            }
            out.push_str("knobs (default = today's behaviour):\n");
            for (name, spec) in &registry.knobs {
                let allowed = match spec["type"].string() {
                    Some("enum") => spec["values"]
                        .array()
                        .unwrap_or(&[])
                        .iter()
                        .map(|v| v.string().unwrap_or(""))
                        .collect::<Vec<_>>()
                        .join(" | "),
                    Some("int") => format!(
                        "{}..{}{}",
                        provenant_core::py::python_str(&spec["min"].value()),
                        provenant_core::py::python_str(&spec["max"].value()),
                        if spec["nullable"].truth() {
                            " | null"
                        } else {
                            ""
                        }
                    ),
                    _ => "true | false".into(),
                };
                let mut floors = String::new();
                for (tier, value) in spec["floor"].object().unwrap_or(&[]) {
                    floors.push_str(&format!(" floor {tier}>={}", shown(value)?));
                }
                out.push_str(&format!("  {name}  [{}]  default {}  ({allowed}){floors}\n      {}\n      trade-off: {}\n",spec["status"].string().unwrap_or(""),shown(&spec["default"])?,spec["effect"].string().unwrap_or(""),spec["trade_off"].string().unwrap_or("")));
            }
            out.push_str("presets:\n");
            for (name, preset) in &registry.presets {
                out.push_str(&format!(
                    "  {name}: {}\n",
                    preset
                        .get("description")
                        .map(|v| v.python_str())
                        .unwrap_or_default()
                ));
            }
        }
        Command::Show {
            resolved,
            task_class,
            risk_tier,
            json,
        } => {
            if !resolved {
                let paths = [
                    (
                        "user",
                        profiles::user_path(context.environment, context.homes)?,
                    ),
                    ("project", profiles::project_path(context.cwd)?),
                ];
                let mut report = Json::default();
                for (name, path) in paths {
                    let layer = registry.read_layer(&path, name)?;
                    report.set(
                        name,
                        Json::Object(vec![
                            (
                                "path".into(),
                                Json::String(crate::pending::path_text(&path)?),
                            ),
                            ("profile".into(), layer.document.clone()),
                        ]),
                    )?;
                    if !json {
                        out.push_str(&format!(
                            "{name}: {}\n  {}\n",
                            path.display(),
                            if layer.document.truth() {
                                ordered::dumps(&layer.document, false, true)?
                            } else {
                                "(none)".into()
                            }
                        ));
                    }
                }
                if *json {
                    return Ok(format!("{}\n", ordered::dumps(&report, true, false)?));
                }
                out.push_str("`provenant profile show --resolved` prints the values in force.\n");
            } else {
                let resolution = profiles::current(
                    registry,
                    context,
                    task_class.as_deref(),
                    risk_tier.as_deref(),
                    None,
                )?;
                if *json {
                    let mut stamp = resolution.stamp_json()?;
                    stamp.set("sources", resolution.sources.clone())?;
                    return Ok(format!("{}\n", ordered::dumps(&stamp, true, false)?));
                }
                let mut scope = String::new();
                if let Some(task) = task_class.as_deref().filter(|s| !s.is_empty()) {
                    scope.push_str(&format!(" task-class {task}"));
                }
                if let Some(tier) = &resolution.risk_tier {
                    scope.push_str(&format!(" risk-tier {tier}"));
                }
                out.push_str(&format!(
                    "preset {}  digest {}{scope}\n",
                    resolution.preset,
                    resolution.digest()?
                ));
                let values = resolution.knobs.object().unwrap_or(&[]);
                let width = values
                    .iter()
                    .map(|(n, _)| n.chars().count())
                    .max()
                    .ok_or_else(|| invalid("resolved knob map is empty"))?;
                let shown = values
                    .iter()
                    .map(|(n, v)| Ok((n, shown(v)?)))
                    .collect::<Result<Vec<_>>>()?;
                let value_width = shown.iter().map(|(_, v)| v.len()).max().unwrap_or(0);
                for (name, value) in shown {
                    out.push_str(&format!(
                        "  {name:width$}  {value:value_width$}  {}\n",
                        resolution.source(name).unwrap_or("default")
                    ));
                }
            }
        }
        Command::Use {
            preset,
            project,
            task_class,
        } => {
            if registry.preset(preset).is_none() {
                return Err(unknown_preset(registry, preset));
            }
            let path = target(*project, context)?;
            profiles::edit_ordered(registry, &path, |d| {
                part(d, task_class.as_deref(), |p| {
                    p.set("preset", Json::String(preset.clone()))
                })
            })?;
            out.push_str(&format!(
                "{}: preset {preset}{}\n",
                path.display(),
                task_class
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(" for {s}"))
                    .unwrap_or_default()
            ));
        }
        Command::Set {
            assignments,
            project,
            task_class,
        } => {
            let values = profiles::parse_assignments(registry, assignments, "set")?;
            let path = target(*project, context)?;
            profiles::edit_ordered(registry, &path, |d| {
                part(d, task_class.as_deref(), |p| {
                    let mut knobs = p.get("knobs").cloned().unwrap_or_default();
                    for (k, v) in values.values.object().unwrap_or(&[]) {
                        knobs.set(k, v.clone())?;
                    }
                    p.set("knobs", knobs)
                })
            })?;
            for (name, value) in values.values.object().unwrap_or(&[]) {
                out.push_str(&format!("{}: {name} = {}\n", path.display(), shown(value)?));
            }
        }
        Command::Reset {
            knobs,
            project,
            task_class,
        } => {
            for name in knobs {
                if name != "preset" && !registry.knob_names().any(|n| n == name) {
                    registry.check(name, &serde_json::Value::Null, "reset")?;
                }
            }
            let path = target(*project, context)?;
            profiles::edit_ordered(registry, &path, |d| {
                part(d, task_class.as_deref(), |p| {
                    let mut values = p.get("knobs").cloned();
                    for name in knobs {
                        if name == "preset" {
                            p.remove("preset");
                        } else if let Some(v) = &mut values {
                            v.remove(name);
                        }
                    }
                    if let Some(values) = values {
                        if values.truth() {
                            p.set("knobs", values)?;
                        } else {
                            p.remove("knobs");
                        }
                    }
                    Ok(())
                })
            })?;
            out.push_str(&format!("{}: reset {}\n", path.display(), knobs.join(", ")));
        }
        Command::Diff {
            left,
            right,
            task_class,
            json,
        } => {
            let resolve = |preset: &str| {
                let bytes = serde_json::to_vec(&serde_json::json!({"preset":preset}))
                    .map_err(|e| invalid(e.to_string()))?;
                registry.resolve(
                    task_class.as_deref(),
                    None,
                    &[profiles::Layer::from_json("diff", &bytes)?],
                )
            };
            let left = resolve(left)?;
            let right = resolve(right)?;
            let rows = Json::Object(
                left.knobs
                    .object()
                    .unwrap_or(&[])
                    .iter()
                    .filter(|(n, v)| {
                        !provenant_core::py::python_json_eq(
                            &v.value(),
                            &right.knobs[n.as_str()].value(),
                        )
                    })
                    .map(|(n, v)| {
                        (
                            n.clone(),
                            Json::Array(vec![v.clone(), right.knobs[n.as_str()].clone()]),
                        )
                    })
                    .collect(),
            );
            if *json {
                return Ok(format!(
                    "{}\n",
                    ordered::dumps(
                        &Json::Object(vec![
                            ("left".into(), left.stamp_json()?),
                            ("right".into(), right.stamp_json()?),
                            ("differs".into(), rows)
                        ]),
                        true,
                        false
                    )?
                ));
            }
            out.push_str(&format!(
                "{} ({}) -> {} ({})\n",
                left.preset,
                left.digest()?,
                right.preset,
                right.digest()?
            ));
            if !rows.truth() {
                out.push_str("  no knob differs\n");
            }
            for (name, values) in rows.object().unwrap_or(&[]) {
                let values = values.array().ok_or_else(|| invalid("invalid diff row"))?;
                let before = values.first().ok_or_else(|| invalid("empty diff row"))?;
                let after = values
                    .get(1)
                    .ok_or_else(|| invalid("incomplete diff row"))?;
                let spec = registry
                    .knobs
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, s)| s)
                    .ok_or_else(|| invalid("diff knob disappeared"))?;
                out.push_str(&format!(
                    "  {name}: {} -> {}  [{}]\n",
                    shown(before)?,
                    shown(after)?,
                    spec["status"].string().unwrap_or("")
                ));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::{Path, PathBuf},
    };
    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .unwrap()
    }
    #[test]
    fn list_diff_show_and_edit_command_bytes_match_python() {
        use clap::Parser;
        let registry = Registry::load(&root()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let space = dir.path().canonicalize().unwrap();
        let project = space.join("project");
        fs::create_dir_all(project.join(".git")).unwrap();
        let instance = space.join("instance");
        fs::create_dir(&instance).unwrap();
        let cases = serde_json::json!([
            ["list"],
            ["list", "--json"],
            ["diff", "economy", "rigorous", "--task-class", "review"],
            ["diff", "balanced", "balanced", "--json"],
            ["show"],
            ["use", "economy"],
            ["set", "effort.shift=1", "council.junior_size=2"],
            [
                "set",
                "review.cross_family=require",
                "--task-class",
                "review",
                "--project"
            ],
            ["show", "--resolved", "--task-class", "review", "--json"],
            ["reset", "effort.shift", "preset"],
            ["show", "--resolved"],
            ["set", "missing=1"],
            ["use", "unknown"],
            ["show", "--task-class", "missing"]
        ]);
        let path = space.join("cases");
        fs::write(&path, serde_json::to_vec(&cases).unwrap()).unwrap();
        let body = format!(
            "import json,os,contextlib,io\nos.environ['PROVENANT_PROFILE_ROOT']=str(p.parent/'instance')\nos.environ['AGENT_FABRIC_PRODUCT_ROOT']={:?}\nos.chdir(p.parent/'project')\nout=[]\nfor args in json.loads(p.read_text()):\n stdout=io.StringIO();stderr=io.StringIO()\n with contextlib.redirect_stdout(stdout),contextlib.redirect_stderr(stderr): code=m.main(args)\n out.append([code,stdout.getvalue(),stderr.getvalue()])\nprint(json.dumps(out))",
            root().to_str().unwrap()
        );
        let Some(old) = crate::oracle::python("scripts/harness_profile.py", &body, &path) else {
            return;
        };
        fs::remove_dir_all(&instance).unwrap();
        fs::create_dir(&instance).unwrap();
        let _ = fs::remove_dir_all(project.join(".agents"));
        let homes = provenant_core::paths::Homes {
            current: Some(space.clone()),
            ..Default::default()
        };
        let env: crate::OrderedMap<String> = [(
            profiles::PROFILE_ROOT_ENV.into(),
            instance.display().to_string(),
        )]
        .into_iter()
        .collect();
        let context = Context {
            cwd: &project,
            environment: &env,
            homes: &homes,
        };
        let actual = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|args| {
                let args = std::iter::once("provenant profile")
                    .chain(args.as_array().unwrap().iter().map(|v| v.as_str().unwrap()));
                let args = Arguments::try_parse_from(args).unwrap();
                let output = run(&registry, &args.command, &context);
                serde_json::json!([output.exit_code, output.stdout, output.stderr])
            })
            .collect::<Vec<_>>();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&old).unwrap(),
            serde_json::json!(actual)
        );
    }
}

#[cfg(test)]
mod description_tests {
    use super::*;
    #[test]
    fn list_formats_non_string_descriptions_like_python() {
        let mut registry=Registry::from_json(br#"{"knobs":{},"presets":{"balanced":{"description":7},"other":{"description":null}}}"#,vec![],vec![]).unwrap();
        registry.presets.push((
            "array".into(),
            Json::Object(vec![(
                "description".into(),
                Json::Array(vec![Json::Bool(true)]),
            )]),
        ));
        let env = crate::OrderedMap::new();
        let homes = provenant_core::paths::Homes::default();
        let context = Context {
            cwd: std::path::Path::new("/"),
            environment: &env,
            homes: &homes,
        };
        let output = run(&registry, &Command::List { json: false }, &context);
        assert!(
            output
                .stdout
                .contains("  balanced: 7\n  other: None\n  array: [True]\n")
        );
    }
}
