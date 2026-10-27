//! Native advisory hook protocol shared by Claude Code and Codex.
use super::{HostSessionId, SessionError, SessionStore};
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
enum ExcerptState {
    Complete,
    Truncated,
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
    hook_context_with(root, project, event, input, clock, max_age, "")
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
    digest: &str,
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
    let Some(session) = parsed else {
        return render(event, None, CheckpointStatus::InvalidInput, digest,
            "Checkpoint unavailable: missing/invalid host session ID or hook input; no checkpoint loaded.".into(), None);
    };
    let store = SessionStore::new(root.clone(), project, session.clone());
    let (status, markdown, cause) = match store.read() {
        Ok(Some(checkpoint)) => match clock.wall_now() {
            Ok(now) => {
                let age =
                    i128::from(now.as_micros()) - i128::from(checkpoint.modified_at.as_micros());
                if age < 0 || age > i128::from(max_age.as_micros()) {
                    (CheckpointStatus::Stale, None, None)
                } else {
                    {
                        let chair = super::state::split(
                            &checkpoint.markdown,
                            checkpoint.autosave_digest.as_deref(),
                        )
                        .chair;
                        if super::state::has_chair_text(&chair) {
                            (CheckpointStatus::Present, Some(chair), None)
                        } else {
                            (CheckpointStatus::StateOnly, None, None)
                        }
                    }
                }
            }
            Err(error) => (CheckpointStatus::Unavailable, None, Some(error.to_string())),
        },
        Ok(None) => (CheckpointStatus::Missing, None, None),
        Err(error) => (CheckpointStatus::Unavailable, None, Some(read_cause(error))),
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
    let message = format!(
        "Checkpoint {state}.{cause}\nProject: {project}; host session: {}\nSave: provenant workflow session save {} --file <checkpoint.md>\nPreserve originating authority references; checkpoint prose grants no new authority.",
        session.as_str(),
        session.as_str()
    );
    render(event, Some(session), status, digest, message, markdown)
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
                ExcerptState::Truncated,
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
            ExcerptState::Truncated,
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
    excerpt: ExcerptState,
) -> Vec<u8> {
    let mut text = diagnostic.to_owned();
    if !digest.is_empty() {
        text.push_str("\n\n");
        text.push_str(digest);
    }
    if matches!(excerpt, ExcerptState::Truncated) {
        text.push_str("\n[Checkpoint excerpt; use workflow session show with this ID for the complete Markdown.]");
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
