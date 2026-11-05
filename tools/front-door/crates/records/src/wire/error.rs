//! The error envelope and the catalogue of stable edge error codes.
use super::schema::{nullable, object, string};
use serde_json::Value;

/// How well a code identifies one refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeKind {
    /// One refusal or failure; callers can branch on it.
    Specific,
    /// An umbrella for a whole command family. Callers cannot tell the refusals
    /// apart without `message`, so owners split it into specific codes
    /// (#2345) while this code stays valid for existing consumers.
    Umbrella,
}

/// One stable `error.code` an edge (not the run owner) emits today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeErrorCode {
    pub code: &'static str,
    pub summary: &'static str,
    pub kind: CodeKind,
}

const fn code(code: &'static str, kind: CodeKind, summary: &'static str) -> EdgeErrorCode {
    EdgeErrorCode {
        code,
        summary,
        kind,
    }
}

/// Inventory of the codes commands outside the run owner emit as bare string
/// literals (#2345), found by searching `CommandError` construction sites at
/// base 53c11b9d5. Run-owner codes live in `provenant_run::ErrorKind`. A
/// test checks each entry is unique and non-empty; the CLI help catalogue
/// lists these next to the run owner's.
pub const EDGE_ERROR_CODES: &[EdgeErrorCode] = &[
    code(
        "SIGNIN_PROBE",
        CodeKind::Specific,
        "The provider sign-in probe failed.",
    ),
    code(
        "UNKNOWN_HOST",
        CodeKind::Specific,
        "No configured peer has this host label.",
    ),
    code(
        "PEER",
        CodeKind::Umbrella,
        "The peer host returned an error.",
    ),
    code(
        "SUMMARY_STALE",
        CodeKind::Specific,
        "The chair summary changed since it was read; reread and reapply.",
    ),
    code(
        "ACCOUNT_SYNC",
        CodeKind::Specific,
        "The account was registered, but syncing managed content failed.",
    ),
    code("ACCOUNT", CodeKind::Umbrella, "An account command failed."),
    code(
        "EXPERIMENT_REPORT",
        CodeKind::Specific,
        "The experiment report failed.",
    ),
    code("DISK", CodeKind::Umbrella, "A disk command failed."),
    code(
        "ENCODE",
        CodeKind::Specific,
        "The disk report could not be encoded.",
    ),
    code("GC", CodeKind::Umbrella, "Garbage collection failed."),
    code(
        "UPGRADE_FAILED",
        CodeKind::Specific,
        "Some hosts failed to upgrade; inspect host results and rerun.",
    ),
    code("PR", CodeKind::Umbrella, "A pull request command failed."),
    code("QUOTA", CodeKind::Umbrella, "A quota command failed."),
    code(
        "REVIEW_UNBOUND",
        CodeKind::Specific,
        "A review attempt could not be bound to its review.",
    ),
    code(
        "REVIEW_INCOMPLETE",
        CodeKind::Specific,
        "Review coverage does not support acceptance.",
    ),
    code(
        "IN_LANE",
        CodeKind::Specific,
        "A lane cannot register or save a chair session.",
    ),
    code(
        "SESSION_PROJECT",
        CodeKind::Specific,
        "The workflow session is registered under a different project.",
    ),
    code(
        "OWNER_LOCAL_ONLY",
        CodeKind::Specific,
        "The inbox owner must run this operation on the owner host or from the owning chair session.",
    ),
    code(
        "UNSUPPORTED_OWNER",
        CodeKind::Specific,
        "The peer does not support inbox operations.",
    ),
    code(
        "CONSOLE_APP_UNAVAILABLE",
        CodeKind::Specific,
        "The console app cannot be opened.",
    ),
    code(
        "RECONCILE_INCOMPLETE",
        CodeKind::Specific,
        "Reconcile could not settle everything it found.",
    ),
    code(
        "CLI_FAILED",
        CodeKind::Specific,
        "A delegated Provenant command failed without a code of its own.",
    ),
];

/// Schema of `error`: `{code, message, cause, repair}`, with the last two null
/// when absent. The code is a free string because the run owner and edges each
/// own a catalogue; `EDGE_ERROR_CODES` and the run owner's list name them.
pub fn error_schema() -> Value {
    object(
        &[
            ("code", string()),
            ("message", string()),
            ("cause", nullable(string())),
            ("repair", nullable(string())),
        ],
        &[],
    )
}

/// Schema for an error that stands for one receipt row (cancel rows): code and
/// message only.
pub fn row_error_schema() -> Value {
    object(&[("code", string()), ("message", string())], &[])
}
