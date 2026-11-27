//! Bundled smoke launch controls. The lifecycle supplies the writer branch and
//! both review bases; a caller checkout never provisions or pins this graph.
use super::*;

pub(super) fn configure(
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
    let requested = parameters.get("write").is_some();
    // The lifecycle supplies the writer branch and both review bases;
    // a caller checkout never provisions or pins this graph.
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
        parameters["write"] = json!(peer_owned_writer || (!is_remote && !worktree.is_empty()));
    }
    if parameters["write"] != json!(true) {
        let reason = if requested {
            "write is not enabled"
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
