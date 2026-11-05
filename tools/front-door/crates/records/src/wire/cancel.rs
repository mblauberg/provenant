//! Cancel receipts.
use super::error::row_error_schema;
use super::schema::{
    array, boolean, choices, constant, integer, object, one_of, open_object, string,
};
use serde::Serialize;
use serde_json::Value;

/// What a cancel request achieved for one attempt. Requested does not prove
/// the process exited. Mirrors the run owner's receipt; a test pins the states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CancelReceiptWire {
    /// Queued work ended without reaching a provider.
    Cancelled,
    AlreadyTerminal,
    Requested,
    AlreadyRequested,
    /// No launch summary or custody to address; reconcile first.
    Unobservable {
        reason: String,
    },
}

/// The stable receipt state names.
pub const CANCEL_STATES: &[&str] = &[
    "cancelled",
    "already_terminal",
    "requested",
    "already_requested",
    "unobservable",
];

/// An error on one attempt's cancel; the other receipts survive it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CancelRowError {
    pub code: String,
    pub message: String,
    pub cause: Option<String>,
    pub repair: Option<String>,
    #[serde(flatten)]
    pub diagnostic: Option<crate::diagnostic::DiagnosticFields>,
}

/// One row of `receipts`: the attempt and either its receipt or its error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum CancelRow {
    Receipt {
        attempt_id: String,
        receipt: CancelReceiptWire,
    },
    Error {
        attempt_id: String,
        error: CancelRowError,
    },
}

/// One run recovery performed after a cancel request.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CancelRecovered {
    pub attempt_id: String,
    pub found: ReconciledWire,
}

/// The run owner's tagged reconciliation result.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReconciledWire {
    Queued,
    Held,
    DeadlineStop,
    Settling {
        phase: String,
    },
    Finished,
    TemporaryRemoved,
    TemporaryRetained {
        path: crate::AbsolutePathWire,
        reason: String,
    },
    Withdrawn,
    Escaped {
        pids: Vec<i64>,
    },
    Uncertain {
        reason: Value,
    },
}

pub fn reconciled_schema() -> Value {
    one_of(vec![
        object(
            &[(
                "state",
                choices(&[
                    "queued",
                    "held",
                    "deadline_stop",
                    "finished",
                    "temporary_removed",
                    "withdrawn",
                ]),
            )],
            &[],
        ),
        object(&[("state", constant("settling")), ("phase", string())], &[]),
        object(
            &[
                ("state", constant("temporary_retained")),
                ("path", string()),
                ("reason", string()),
            ],
            &[],
        ),
        object(
            &[("state", constant("escaped")), ("pids", array(integer()))],
            &[],
        ),
        object(
            &[("state", constant("uncertain")), ("reason", open_object())],
            &[],
        ),
    ])
}

/// `data` of `cancel`. The members after `receipts` appear only when the
/// request was accepted (`recovered`) or `--wait` was given (the rest, the
/// same fields a lane wait reports).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CancelData {
    pub operation_id: String,
    pub receipts: Vec<CancelRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovered: Option<Vec<CancelRecovered>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lanes: Option<Vec<super::lane::LaneRow>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition_met: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timed_out: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

pub fn cancel_receipt_schema() -> Value {
    one_of(vec![
        object(&[("state", constant("cancelled"))], &[]),
        object(
            &[(
                "state",
                choices(&["already_terminal", "requested", "already_requested"]),
            )],
            &[],
        ),
        object(
            &[("state", constant("unobservable")), ("reason", string())],
            &[],
        ),
    ])
}

pub fn cancel_data_schema() -> Value {
    let row = one_of(vec![
        object(
            &[
                ("attempt_id", string()),
                ("receipt", cancel_receipt_schema()),
            ],
            &[],
        ),
        object(
            &[("attempt_id", string()), ("error", row_error_schema())],
            &[],
        ),
    ]);
    object(
        &[("operation_id", string()), ("receipts", array(row))],
        &[
            (
                "recovered",
                array(object(
                    &[("attempt_id", string()), ("found", reconciled_schema())],
                    &[],
                )),
            ),
            ("lanes", array(super::lane::lane_row_schema())),
            ("condition_met", boolean()),
            ("timed_out", boolean()),
            ("reason", string()),
        ],
    )
}
