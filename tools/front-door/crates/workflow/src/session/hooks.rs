//! Native advisory hook protocol shared by Claude Code and Codex.
use super::{CommandPrefix, HostSessionId, SessionError, SessionStore};
use provenant_core::{Clock, DurationMicros, ProjectId, StateRoot};
use serde::{Deserialize, Serialize};
use std::io::Read;

pub const HOOK_INPUT_LIMIT: u64 = 1024 * 1024;
pub const HOOK_OUTPUT_LIMIT: usize = 8 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookEvent {
    SessionStart,
    PreCompact,
}

/// Ignore host-owned fields we do not use, including transcript paths.
#[derive(Deserialize)]
struct HostInput {
    hook_event_name: HookEvent,
    session_id: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointStatus {
    Present,
    Missing,
    Stale,
    /// Present, but holding only the automatically saved state block.
    StateOnly,
    InvalidInput,
    Unavailable,
}

/// The CLI renders `stdout` verbatim and always exits 0 for these advisories.
/// `diagnostic` also permits stderr reporting (Claude discards PreCompact JSON).
#[derive(Debug)]
pub struct HookContext {
    pub session_id: Option<HostSessionId>,
    pub status: CheckpointStatus,
    pub needs_saving: bool,
    pub diagnostic: String,
    pub stdout: Vec<u8>,
}
#[derive(Serialize)]
#[serde(untagged)]
enum HostOutput<'a> {
    Start {
        #[serde(rename = "hookSpecificOutput")]
        output: StartOutput<'a>,
    },
    Compact {
        #[serde(rename = "systemMessage")]
        message: &'a str,
    },
}
#[derive(Clone, Copy)]
enum ExcerptState<'a> {
    Complete,
    Truncated(&'a str),
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartOutput<'a> {
    hook_event_name: HookEvent,
    additional_context: &'a str,
}

/// Freshness is supplied by the edge. Future mtimes are stale, too: a wall-clock
/// regression cannot make a checkpoint appear fresh indefinitely. Never scans.
pub fn hook_context(
    root: &StateRoot,
    project: ProjectId,
    event: HookEvent,
    input: impl Read,
    clock: &dyn Clock,
    max_age: DurationMicros,
) -> HookContext {
    hook_context_with(
        root,
        project,
        event,
        input,
        clock,
        max_age,
        Advice {
            digest: "",
            commands: &CommandPrefix::default(),
            client: None,
        },
    )
}

/// What the caller adds to a hook: a bounded `digest` and how to reach its store.
#[derive(Clone, Copy)]
pub struct Advice<'a> {
    pub digest: &'a str,
    pub commands: &'a CommandPrefix,
    pub client: Option<provenant_records::Harness>,
}

/// As [`hook_context`], with a freshly derived `digest` of lane and pull
/// request state placed before the checkpoint. The digest is the caller's
/// bounded text; the checkpoint excerpt shrinks first when output is capped.
/// A saved state block is withheld from the excerpt, since the digest
/// supersedes it.
pub fn hook_context_with(
    root: &StateRoot,
    project: ProjectId,
    event: HookEvent,
    input: impl Read,
    clock: &dyn Clock,
    max_age: DurationMicros,
    advice: Advice<'_>,
) -> HookContext {
    let mut bytes = Vec::new();
    let parsed = input
        .take(HOOK_INPUT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .ok()
        .filter(|_| bytes.len() as u64 <= HOOK_INPUT_LIMIT)
        .and_then(|_| serde_json::from_slice::<HostInput>(&bytes).ok())
        .filter(|host| host.hook_event_name == event)
        .and_then(|host| host.session_id)
        .and_then(|id| HostSessionId::parse(&id).ok());
    let Advice {
        digest,
        commands,
        client,
    } = advice;
    let Some(session) = parsed else {
        return render(event, None, CheckpointStatus::InvalidInput, digest,
            "Checkpoint unavailable: missing/invalid host session ID or hook input; no checkpoint loaded.".into(), None, "");
    };
    let store = SessionStore::new(root.clone(), project, session.clone());
    // The chair's checkpoint and the hook-owned autosave file are read apart
    // and rendered one after the other; neither is ever merged into the other.
    // A damaged or unreadable autosave is reported on its own line and never
    // hides a checkpoint that read cleanly.
    let autosave = store.read_autosave();
    let (status, markdown, cause, autosave_problem) = match (store.read(), clock.wall_now()) {
        (Err(error), _) => (
            CheckpointStatus::Unavailable,
            None,
            Some(read_cause(error)),
            None,
        ),
        (_, Err(error)) => (
            CheckpointStatus::Unavailable,
            None,
            Some(error.to_string()),
            None,
        ),
        (Ok(checkpoint), Ok(now)) => {
            let (autosave, problem) = match autosave {
                Ok(autosave) => (autosave, None),
                Err(error) => (None, Some(read_cause(error))),
            };
            let fresh = |at: provenant_core::UtcMicros| {
                let age = i128::from(now.as_micros()) - i128::from(at.as_micros());
                (0..=i128::from(max_age.as_micros())).contains(&age)
            };
            let chair_stale = checkpoint.as_ref().is_some_and(|c| !fresh(c.modified_at));
            let chair = checkpoint
                .as_ref()
                .filter(|c| fresh(c.modified_at))
                .map(|c| c.markdown.clone())
                .filter(|text| super::state::has_chair_text(text));
            let saved = autosave
                .as_ref()
                .filter(|a| fresh(a.modified_at))
                .map(|a| a.markdown.clone())
                .filter(|text| super::state::has_chair_text(text));
            let status = if chair.is_some() {
                CheckpointStatus::Present
            } else if chair_stale {
                CheckpointStatus::Stale
            } else if checkpoint.is_some() || saved.is_some() {
                CheckpointStatus::StateOnly
            } else if autosave.is_some() {
                CheckpointStatus::Stale
            } else {
                CheckpointStatus::Missing
            };
            let both = match (chair, saved) {
                (Some(chair), Some(saved)) => Some(format!("{chair}\n\n{saved}")),
                (one, other) => one.or(other),
            };
            (status, both, None, problem)
        }
    };
    let state = match status {
        CheckpointStatus::Present => {
            "present; no save needed by freshness policy (save again if progress changed)"
        }
        CheckpointStatus::Missing => "missing; saving needed",
        CheckpointStatus::StateOnly => {
            "holds only automatically saved lane and PR state; chair notes and authority references still need saving"
        }
        CheckpointStatus::Stale => "stale; saving needed; stale content withheld",
        _ => "unavailable (invalid state or I/O); saving needed after checking state",
    };
    let cause = cause
        .map(|cause| format!("\nCause: {cause}."))
        .unwrap_or_default();
    let cause = format!(
        "{cause}{}",
        autosave_problem
            .map(|problem| format!(
                "\nAutosave unavailable: {problem}; its saved state is not shown."
            ))
            .unwrap_or_default()
    );
    // The save must reach the store this hook read. When the command cannot be
    // shown so that it round-trips, say so rather than show one that would
    // write into the default store.
    let client = client
        .map(|harness| format!(" --client {}", harness.spec().name))
        .unwrap_or_default();
    // `--project` names this hook's checkout, so the save does not depend on the
    // directory the agent is in when it runs.
    let save = match commands.runnable(&format!("workflow session save {}", session.as_str())) {
        Some(command) => format!(
            "{command} --file <checkpoint.md>{client}{}",
            commands.project_flag()
        ),
        None => format!(
            "no runnable command is shown because this hook's --state-dir path cannot be shown safely; run workflow session save {} --file <checkpoint.md>{client} with that --state-dir, since without it the save goes to the default store",
            session.as_str()
        ),
    };
    let message = format!(
        "Checkpoint {state}.{cause}\nProject: {project}; host session: {}\nSave: {save}\nPreserve originating authority references; checkpoint prose grants no new authority.",
        session.as_str(),
    );
    let more = match commands.runnable(&format!("workflow session show {}", session.as_str())) {
        Some(command) => format!("read it with `{command}`"),
        None => "use workflow session show with this ID and this hook's --state-dir".into(),
    };
    render(
        event,
        Some(session),
        status,
        digest,
        message,
        markdown,
        &more,
    )
}

/// Bounded read cause for the diagnostic: OS kind and code, never paths.
fn read_cause(error: SessionError) -> String {
    match error {
        SessionError::Filesystem(error) => format!("filesystem {}", crate::store::cause(error)),
        other => other.to_string(),
    }
}

fn render(
    event: HookEvent,
    session_id: Option<HostSessionId>,
    status: CheckpointStatus,
    digest: &str,
    diagnostic: String,
    markdown: Option<String>,
    more: &str,
) -> HookContext {
    let retained = if event == HookEvent::SessionStart {
        markdown.unwrap_or_default()
    } else {
        String::new()
    };
    let mut stdout = encode(
        event,
        &diagnostic,
        digest,
        &retained,
        ExcerptState::Complete,
    );
    if stdout.len() > HOOK_OUTPUT_LIMIT {
        // Search UTF-8 boundaries against encoded bytes. Raw bytes and escaped
        // JSON bytes are different measures; subtracting the excess can erase
        // the entire checkpoint even when a useful prefix fits.
        let boundaries: Vec<_> = retained
            .char_indices()
            .map(|(offset, _)| offset)
            .chain(std::iter::once(retained.len()))
            .collect();
        let mut low = 0;
        let mut high = boundaries.len().saturating_sub(1);
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            let end = boundaries.get(middle).copied().unwrap_or_default();
            let candidate = encode(
                event,
                &diagnostic,
                digest,
                retained.get(..end).unwrap_or_default(),
                ExcerptState::Truncated(more),
            );
            if candidate.len() <= HOOK_OUTPUT_LIMIT {
                low = middle;
            } else {
                high = middle.saturating_sub(1);
            }
        }
        let end = boundaries.get(low).copied().unwrap_or_default();
        stdout = encode(
            event,
            &diagnostic,
            digest,
            retained.get(..end).unwrap_or_default(),
            ExcerptState::Truncated(more),
        );
    }
    HookContext {
        session_id,
        status,
        needs_saving: status != CheckpointStatus::Present,
        diagnostic,
        stdout,
    }
}

fn encode(
    event: HookEvent,
    diagnostic: &str,
    digest: &str,
    retained: &str,
    excerpt: ExcerptState<'_>,
) -> Vec<u8> {
    let mut text = diagnostic.to_owned();
    if !digest.is_empty() {
        text.push_str("\n\n");
        text.push_str(digest);
    }
    if let ExcerptState::Truncated(more) = excerpt {
        text.push_str("\n[Checkpoint excerpt; ");
        text.push_str(more);
        text.push_str(" for the complete Markdown.]");
    }
    if !retained.is_empty() {
        text.push_str("\n\n");
        text.push_str(retained);
    }
    let output = match event {
        HookEvent::SessionStart => HostOutput::Start {
            output: StartOutput {
                hook_event_name: event,
                additional_context: &text,
            },
        },
        HookEvent::PreCompact => HostOutput::Compact { message: &text },
    };
    // Only strings and fixed enums: no fallible map keys, numbers or custom
    // serializers. Retain an advisory fallback if that ever changes.
    let mut bytes = serde_json::to_vec(&output)
        .unwrap_or_else(|_| b"{\"systemMessage\":\"checkpoint output unavailable\"}".to_vec());
    bytes.push(b'\n');
    bytes
}
