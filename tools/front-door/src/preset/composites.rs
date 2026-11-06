//! Template submission adapter. Preset names are aliases, not a second schema.
use super::*;
use provenant_fabric_runtime::graph::{self, Compiler, GraphDocument};

pub const PRESET_TEMPLATES: &[(&str, &str)] = &[
    ("single", "task-once"),
    ("scope", "plan-once"),
    ("chain", "task-chain"),
    ("fan-out", "task-fan-out"),
    ("fan-out-synthesise", "task-fan-out-synthesise"),
    ("scout-swarm", "explore-swarm"),
    ("generator-evaluator", "deliver"),
    ("review", "review-panel"),
    ("big-batch", "batch"),
    ("scope-then-run", "plan-then-deliver"),
    ("deliver", "deliver"),
];

/// Routes and work bindings belong to individual named batch items.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositeLane {
    pub name: String,
    pub worktree: String,
    pub scope: Vec<String>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub brief: Option<String>,
}

/// Lower a template or preset alias for submission. The host supplies
/// resolver lists, including `review-panel`. This creates no Git refs.
#[allow(dead_code, reason = "entry point for the template run interface")]
pub fn expand_composite(
    layers: &Layers,
    name: &str,
    parameters: &Value,
    lanes: &[CompositeLane],
    resolvers: &BTreeMap<String, Vec<String>>,
) -> Result<Expansion, String> {
    let template = PRESET_TEMPLATES
        .iter()
        .find_map(|(preset, template)| (*preset == name).then_some(*template))
        .unwrap_or(name);
    let mut parameters = parameters.clone();
    let parameters = parameters
        .as_object_mut()
        .ok_or("template parameters must be a table")?;
    if template == "batch" && !lanes.is_empty() {
        let names: BTreeSet<_> = lanes.iter().map(|lane| &lane.name).collect();
        if names.len() != lanes.len() || names.iter().any(|name| name.trim().is_empty()) {
            return Err("batch lane names must be nonempty and unique".into());
        }
        parameters.insert(
            "items".into(),
            json!(lanes.iter().map(|lane| &lane.name).collect::<Vec<_>>()),
        );
    }
    let compiler = Compiler {
        library: bundled_templates(),
        resolvers,
    };
    let mut node = compiler.compile(&json!({"id":"work", "use":template, "with":parameters}))?;
    if template == "batch" && !lanes.is_empty() {
        let children = &mut node
            .composite
            .as_mut()
            .ok_or("batch must be composite")?
            .nodes;
        let slices = children
            .iter_mut()
            .find(|child| child.id == "slices")
            .and_then(|child| child.composite.as_mut())
            .ok_or("batch must have slices")?;
        for (member, lane) in slices.nodes.iter_mut().zip(lanes) {
            member.worktree = Some(lane.worktree.clone());
            member.write_scope = lane.scope.clone();
            if let Some(tier) = &lane.tier {
                if layers
                    .known_routes
                    .as_ref()
                    .is_some_and(|routes| !routes.contains(tier))
                {
                    return Err(format!("unknown lane tier: {tier}"));
                }
                member.route =
                    Some(serde_json::from_value(json!({"alias":tier})).map_err(|e| e.to_string())?);
            }
            if let Some(brief) = &lane.brief {
                member.prompt = brief.clone();
            }
        }
    }
    let digest = provenant_records::CanonicalV1::file_digest(
        &serde_json::to_vec(&node).map_err(|e| e.to_string())?,
    )
    .to_string();
    let mut document = GraphDocument {
        schema_version: 1,
        graph_id: format!("template-{}", &digest[7..39]),
        nodes: vec![node],
        review_base: None,
        context: None,
        extra: Default::default(),
        limits: None,
        integration_branch: (template == "batch").then(|| {
            parameters
                .get("integration_branch")
                .and_then(Value::as_str)
                .unwrap_or("batch/integration")
                .to_owned()
        }),
        delivery: None,
        decision_session: None,
    };
    document.schema_version = u64::from(graph::minimum_schema_version(&document));
    graph::patch::scan_for_secrets(&serde_json::to_string(&document).map_err(|e| e.to_string())?)?;
    document.admit(&layers.graph_limits)?;
    let mut document = serde_json::to_value(document).map_err(|e| e.to_string())?;
    document["provenance"] = json!({"preset":name,"template":template});
    Ok(Expansion {
        document,
        worktree: parameters
            .get("worktree")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from),
        review_shape: None,
        origins: BTreeMap::new(),
        repo_files: Vec::new(),
        roles: BTreeMap::new(),
        substitutions: Vec::new(),
        warnings: Vec::new(),
        plan: None,
    })
}

/// Create a local integration ref without moving the checkout or resetting an
/// existing integration head. Called only when executing, never by explain.
pub(super) fn ensure_integration_branch(directory: &Path, branch: &str) -> Result<(), String> {
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    use std::ffi::OsString;
    let executable =
        provenant_os::git::git_executable().ok_or("Git unavailable for integration branch")?;
    let executor = SystemExecutor::new(1).map_err(|error| error.to_string())?;
    let run = |args: &[&str]| {
        let mut argv = vec![executable.clone().into_os_string()];
        argv.extend(args.iter().map(OsString::from));
        let env: Vec<(OsString, OsString)> = [
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_SYSTEM", "/dev/null"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_NO_REPLACE_OBJECTS", "1"),
            ("LC_ALL", "C"),
        ]
        .map(|(key, value)| (key.into(), value.into()))
        .into();
        let mut options = ExecOptions::new(&argv, directory, std::time::Duration::from_secs(30));
        options.env = Some(&env);
        let output = executor
            .execute(options)
            .map_err(|error| error.to_string())?;
        if output.timed_out {
            return Err("Git timed out preparing integration branch".to_owned());
        }
        Ok(output)
    };
    let valid = run(&["check-ref-format", "--branch", branch])?;
    if valid.termination != Termination::Exit(0)
        || branch.starts_with('-')
        || branch.starts_with("origin/")
    {
        return Err("integration branch must be a valid local branch name".into());
    }
    let reference = format!("refs/heads/{branch}");
    let existing = run(&["show-ref", "--verify", "--quiet", &reference])?;
    if existing.termination == Termination::Exit(0) {
        return Ok(());
    }
    if existing.termination != Termination::Exit(1) {
        return Err("cannot inspect integration branch".into());
    }
    let result = run(&["branch", "--", branch, "HEAD"])?;
    if result.termination == Termination::Exit(0) {
        Ok(())
    } else {
        Err(format!(
            "cannot create integration branch: {}",
            String::from_utf8_lossy(&result.stderr.bytes).trim()
        ))
    }
}

#[cfg(test)]
#[path = "composite_tests.rs"]
mod tests;
