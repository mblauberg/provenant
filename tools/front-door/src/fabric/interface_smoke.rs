//! Bundled smoke launch controls and review range binding.
use super::*;

pub(super) fn configure(
    factory: &Factory,
    input: &Value,
    parameters: &mut Value,
    overrides: &Value,
    smoke_writer: &Value,
    first_control: Option<&str>,
    input_warnings: &mut Vec<String>,
) -> Result<(), FacadeError> {
    let is_remote = input
        .get("host")
        .and_then(Value::as_str)
        .is_some_and(|h| !h.is_empty() && !matches!(h, "local" | "localhost"));
    let controls = smoke_writer
        .as_object()
        .is_some_and(|node| !node.is_empty());
    if controls {
        // A remote graph must not retain or transmit a caller path.
        // `--branch`/`--base` name a worktree the peer creates in its
        // own clone, so they are admitted; only a caller path is not.
        if is_remote && first_control == Some("worktree") {
            return Err(FacadeError::invalid(InvalidInputDetail::Field(
                "worktree".into(),
            )));
        }
        // A control and a `--set` of the same binding are two
        // answers to one question.
        if ["worktree", "write_attempt"]
            .iter()
            .any(|name| overrides.get(*name).is_some())
        {
            return Err(FacadeError::invalid(InvalidInputDetail::Field(
                "set".into(),
            )));
        }
        parameters["worktree"] = smoke_writer["worktree"].clone();
        if let Some(attempt) = smoke_writer.get("attempt") {
            parameters["write_attempt"] = attempt.clone();
        }
    }
    // A peer-owned writer is provisioned from a branch and a base ref,
    // never from a caller path. Drop the host path while retaining the
    // branch intent so the peer can create its own worktree.
    if is_remote {
        parameters["worktree"] = json!("");
    }
    let requested = parameters.get("smoke-write").is_some();
    let write_disabled = requested && parameters["smoke-write"] == json!(false);
    // A remote launch carries no caller worktree. Provision a scratch
    // branch from a sourced default so verification exercises the
    // remote writer even when the caller supplies no --branch; an
    // explicit --branch/--base or --set write_attempt still wins.
    if is_remote
        && !write_disabled
        && !parameters
            .get("write_attempt")
            .and_then(|attempt| attempt.get("new_worktree"))
            .is_some()
    {
        let branch = input
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(|name| format!("smoke/{name}"))
            .unwrap_or_else(|| "smoke/verify".to_owned());
        let explicit_base = input
            .get("base")
            .and_then(Value::as_str)
            .filter(|base| !base.is_empty())
            .map(str::to_owned);
        let base = explicit_base.as_deref().unwrap_or(DEFAULT_WRITER_BASE);
        let mut attempt = parameters
            .get("write_attempt")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let object = attempt.as_object_mut().ok_or_else(|| {
            FacadeError::invalid(InvalidInputDetail::Field("write_attempt".into()))
        })?;
        let mut provision = json!({"branch":branch,"base":base});
        if explicit_base.is_none() {
            provision["default_base"] = json!(true);
        }
        object.insert("new_worktree".into(), provision);
        parameters["write_attempt"] = attempt;
        input_warnings.push(format!(
                    "remote smoke writer uses scratch branch {branch} from {base} (source: default smoke placement); pass --branch B --base REF to override"
                ));
    }
    // Review reads the writer's head, so a launch with no writer has
    // no scratch commit to review. Skip it with a sourced reason
    // instead of falling back to the caller checkout.
    let writer_will_run = !write_disabled
        && (is_remote
            || parameters
                .get("worktree")
                .and_then(Value::as_str)
                .is_some_and(|path| !path.is_empty())
            || input
                .get("branch")
                .and_then(Value::as_str)
                .is_some_and(|branch| !branch.is_empty())
            || parameters
                .get("write_attempt")
                .and_then(|attempt| attempt.get("new_worktree"))
                .is_some());
    let explicit_diff_base = parameters
        .get("diff_base")
        .and_then(Value::as_str)
        .filter(|base| !base.is_empty())
        .map(str::to_owned);
    if let Some(base) = &explicit_diff_base {
        input_warnings.push(format!(
                    "smoke review uses base {base} (source: --set diff_base); the writer's scratch base is overridden"
                ));
    }
    if !writer_will_run {
        parameters["smoke-review"] = json!(false);
        input_warnings.push(
                    "smoke skipped review: no writer scratch commit; pass --worktree PATH or --branch B to run one".into(),
                );
    } else if explicit_diff_base.is_none() {
        // The review reads the writer's head, so its base is the commit
        // the writer branched from. That bounds the reviewed diff to
        // the writer's own scratch commit instead of whatever commit
        // the caller happens to be on.
        let explicit_base = input
            .get("base")
            .and_then(Value::as_str)
            .filter(|base| !base.is_empty());
        let branch_named = input
            .get("branch")
            .and_then(Value::as_str)
            .is_some_and(|branch| !branch.is_empty());
        let resolve = |reference: &str, cwd: &Path| {
            provenant_os::git::git_executable()
                .and_then(|git| provenant_run::resolve_git_base(&git, cwd, reference).ok())
        };
        // A named branch may already have a linked checkout. In that
        // case the writer continues from that checkout's accumulated
        // history, so anchor review at its HEAD immediately before the
        // write instead of the caller's default branch.
        let existing_branch_head = if branch_named
            && !is_remote
            && parameters
                .get("write_attempt")
                .and_then(|attempt| attempt.get("new_worktree"))
                .is_none()
        {
            parameters
                .get("worktree")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
                .and_then(|path| {
                    let path = Path::new(path);
                    let path = if path.is_absolute() {
                        path.to_owned()
                    } else {
                        factory.context.project_root.join(path)
                    };
                    resolve("HEAD", &path)
                })
        } else {
            None
        };
        let uses_existing_branch_head = existing_branch_head.is_some();
        let provisioned_base = parameters
            .get("write_attempt")
            .and_then(|attempt| attempt.get("new_worktree"))
            .and_then(|provision| provision.get("base"))
            .and_then(Value::as_str)
            .filter(|base| !base.is_empty());
        let base_ref = explicit_base
            .map(str::to_owned)
            .or_else(|| branch_named.then(|| DEFAULT_WRITER_BASE.to_owned()))
            .or_else(|| provisioned_base.map(str::to_owned));
        let based = existing_branch_head.or_else(|| {
            base_ref
                .as_deref()
                .and_then(|reference| resolve(reference, &factory.cwd))
        });
        let worktree = parameters
            .get("worktree")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty());
        let base = based.or_else(|| {
            worktree.and_then(|path| {
                let path = Path::new(path);
                let path = if path.is_absolute() {
                    path.to_owned()
                } else {
                    factory.context.project_root.join(path)
                };
                resolve("HEAD", &path)
            })
        });
        if let Some(base) = base {
            parameters["diff_base"] = json!(base);
            input_warnings.push(format!(
                "smoke review uses base {base} (source: {}); pass --set diff_base=BASE to override",
                if uses_existing_branch_head {
                    "existing smoke writer checkout HEAD"
                } else {
                    "smoke writer base"
                }
            ));
        } else {
            parameters["smoke-review"] = json!(false);
            input_warnings.push(
                        "smoke skipped review: the writer's base could not be resolved; pass --set diff_base=BASE to pin a range".into(),
                    );
        }
    }
    let peer_owned_writer = is_remote
        && parameters
            .get("write_attempt")
            .and_then(|attempt| attempt.get("new_worktree"))
            .is_some();
    if !requested {
        let worktree = parameters
            .get("worktree")
            .and_then(Value::as_str)
            .unwrap_or("");
        parameters["smoke-write"] =
            json!(peer_owned_writer || (!is_remote && !worktree.is_empty()));
    }
    if parameters["smoke-write"] != json!(true) {
        let reason = if requested {
            "smoke-write is not enabled"
        } else if is_remote {
            "remote placement needs --branch B to provision a peer-owned worktree; caller paths are not accepted"
        } else {
            "no writer worktree supplied; pass --worktree PATH or --branch B to run it"
        };
        input_warnings.push(format!(
            "smoke skipped node write: {reason}; other eligible nodes still run"
        ));
    }
    Ok(())
}
