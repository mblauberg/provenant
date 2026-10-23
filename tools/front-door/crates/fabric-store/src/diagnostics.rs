//! Diagnostic policy over a private, consistent snapshot supplied by the supervisor.
use crate::*;
use std::collections::BTreeSet;

const REQUIRED_TABLES: &[&str] = &[
    "agents",
    "messages",
    "deliveries",
    "delivery_claims",
    "teams",
    "team_members",
    "tasks",
    "task_dependencies",
    "activity",
    "work_claims",
    "landing_leases",
    "run_observations",
];
const REQUIRED_CLAIM_COLUMNS: &[&str] = &[
    "message_id",
    "project",
    "recipient_id",
    "claim_id",
    "claimed_at",
    "expires_at",
];
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticStatus { Absent, Ok, Error }
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseDiagnostic {
    pub status: DiagnosticStatus,
    pub exists: bool,
    pub read_only: bool,
    pub database: String,
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counts: Option<DiagnosticCounts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<DiagnosticCheck>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
/// Field order is observable in the CLI's JSON.stringify output.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticCounts {
    pub agents: i64,
    pub messages: i64,
    pub pending_deliveries: i64,
    pub tasks: i64,
    pub activity: i64,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct DiagnosticCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}
#[derive(Clone, Copy)]
pub enum DiagnosticMode {
    Status,
    Doctor,
}

/// Inspect only an isolated snapshot, never a live Fabric database.
/// Snapshot creation/version checks/cleanup await the safe filesystem owner.
pub fn inspect_snapshot(
    db: &Connection,
    database: &str,
    project: &str,
    mode: DiagnosticMode,
) -> DatabaseDiagnostic {
    let mut report = DatabaseDiagnostic {
        status: DiagnosticStatus::Ok,
        exists: true,
        read_only: true,
        database: database.into(),
        project: project.into(),
        counts: None,
        checks: None,
        error: None,
    };
    match inspect_policy(db, project, mode) {
        Ok((counts, checks)) => {
            if checks.as_ref().is_some_and(|c| c.iter().any(|c| !c.ok)) {
                report.status = DiagnosticStatus::Error;
            }
            report.counts = Some(counts);
            report.checks = checks;
        }
        Err(error) => {
            report.status = DiagnosticStatus::Error;
            report.error = Some(error.to_string());
        }
    }
    report
}
type DiagnosticPolicy = (DiagnosticCounts, Option<Vec<DiagnosticCheck>>);
fn inspect_policy(
    db: &Connection,
    project: &str,
    mode: DiagnosticMode,
) -> Result<DiagnosticPolicy> {
    db.pragma_update(None, "query_only", "ON")?;
    let tables = db
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    let mut counts = DiagnosticCounts::default();
    for (slot, table, extra) in [
        (&mut counts.agents, "agents", ""),
        (&mut counts.messages, "messages", ""),
        (
            &mut counts.pending_deliveries,
            "deliveries",
            "AND read_at IS NULL",
        ),
        (&mut counts.tasks, "tasks", ""),
        (&mut counts.activity, "activity", ""),
    ] {
        let n = if tables.contains(table) {
            db.query_row(
                &format!("SELECT count(*) FROM {table} WHERE project = ? {extra}"),
                [project],
                |r| r.get(0),
            )?
        } else {
            0
        };
        *slot = n;
    }
    if matches!(mode, DiagnosticMode::Status) {
        return Ok((counts, None));
    }
    let missing: Vec<_> = REQUIRED_TABLES
        .iter()
        .copied()
        .filter(|t| !tables.contains(*t))
        .collect();
    let claim_columns = db
        .prepare("PRAGMA table_info(delivery_claims)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    let missing_columns: Vec<_> = REQUIRED_CLAIM_COLUMNS
        .iter()
        .copied()
        .filter(|c| !claim_columns.contains(*c))
        .collect();
    let integrity = db
        .prepare("PRAGMA quick_check")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let foreign_keys = db
        .prepare("PRAGMA foreign_key_check")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let collisions = if tables.contains("agents") && tables.contains("teams") {
        db.prepare("SELECT a.agent_id FROM agents a JOIN teams t ON t.project = a.project AND t.team_id = a.agent_id WHERE a.project = ? ORDER BY a.agent_id")?.query_map([project], |r| r.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    let checks = vec![
        DiagnosticCheck {
            name: "schema".into(),
            ok: missing.is_empty(),
            detail: if missing.is_empty() {
                format!("{} required tables present", REQUIRED_TABLES.len())
            } else {
                format!("missing tables: {}", missing.join(", "))
            },
        },
        DiagnosticCheck {
            name: "integrity".into(),
            ok: integrity.iter().all(|s| s == "ok"),
            detail: integrity.join("; "),
        },
        DiagnosticCheck {
            name: "delivery-claim-schema".into(),
            ok: missing_columns.is_empty(),
            detail: if missing_columns.is_empty() {
                "delivery claim columns present".into()
            } else {
                format!("missing columns: {}", missing_columns.join(", "))
            },
        },
        DiagnosticCheck {
            name: "foreign-keys".into(),
            ok: foreign_keys.is_empty(),
            detail: if foreign_keys.is_empty() {
                "no foreign-key violations".into()
            } else {
                foreign_keys
                    .iter()
                    .map(|(table, row, parent)| {
                        format!(
                            "{table} row {} -> {parent}",
                            row.map(|r| r.to_string()).unwrap_or_else(|| "null".into())
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            },
        },
        DiagnosticCheck {
            name: "recipient-namespace".into(),
            ok: collisions.is_empty(),
            detail: if collisions.is_empty() {
                "agent labels and team ids are distinct".into()
            } else {
                format!(
                    "agent/team ids overlap (team-first routing): {}",
                    collisions.join(", ")
                )
            },
        },
    ];
    Ok((counts, Some(checks)))
}
