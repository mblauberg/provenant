//! Advisory report of uncommitted work in worktrees no live lane holds.
//!
//! This is state reporting only. Nothing here refuses a command, signals a
//! process or changes removal authority. A worktree with edits is shown so a
//! chair can adopt it before cleanup; gc keeps its ordinary dirty guard.
use provenant_confinement::worktrees::{self, Occupancy, WorktreeCondition, WorktreeContext};
use provenant_core::{CancelToken, Clock, Roots, WallTime};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// One unheld worktree with uncommitted edits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct StrandedWork {
    pub path: PathBuf,
    pub branch: Option<String>,
    /// Best-effort `#<number>` read from a branch name, when one is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    pub files: usize,
    pub lines: usize,
    /// Human age of the newest edit, or `-` when no file time was available.
    pub age: String,
    #[serde(skip)]
    pub newest_edit_micros: Option<i64>,
}

impl StrandedWork {
    /// The command that adopts this work once its issue has landed.
    pub fn next_command(&self) -> String {
        format!("provenant run implement --worktree {}", self.path.display())
    }
}

/// Read an issue or PR number from a branch name when it carries one.
pub(crate) fn linked_reference(branch: &str) -> Option<String> {
    let bytes = branch.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_digit() && (index == 0 || !bytes[index - 1].is_ascii_digit()) {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            let digits = &branch[start..index];
            if (3..=7).contains(&digits.len()) && !digits.starts_with('0') {
                return Some(format!("#{digits}"));
            }
            continue;
        }
        index += 1;
    }
    None
}

fn age_text(micros: i64) -> String {
    let seconds = micros.max(0) / 1_000_000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

fn count_text(count: usize, singular: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {singular}s")
    }
}

/// One advisory line per stranded worktree, plus the section heading.
pub(crate) fn human_lines(rows: &[StrandedWork]) -> Vec<String> {
    if rows.is_empty() {
        return Vec::new();
    }
    let mut lines = vec!["stranded work:".to_owned()];
    for row in rows {
        let branch = row.branch.as_deref().unwrap_or("-");
        let issue = row
            .issue
            .as_deref()
            .map(|issue| format!(" {issue}"))
            .unwrap_or_default();
        lines.push(format!(
            "  {branch}{issue}: {}, {}, {} old: {}",
            count_text(row.files, "file"),
            count_text(row.lines, "line"),
            row.age,
            row.path.display()
        ));
        lines.push(format!("    next: {}", row.next_command()));
    }
    lines
}

/// A shorter form for the compact status summary.
pub(crate) fn summary_lines(rows: &[StrandedWork]) -> Vec<String> {
    let mut lines: Vec<String> = human_lines(rows).into_iter().take(9).collect();
    if rows.len() > 4 {
        lines.push(format!(
            "  {} more stranded worktree(s); use --full to list every row",
            rows.len() - 4
        ));
    }
    lines
}

/// A census-reported live writer or a process observation is a holder. An
/// unknown census is not evidence of no holder, so it is not reported.
fn unheld(occupancy: &Occupancy) -> bool {
    matches!(occupancy, Occupancy::Available)
}

/// Survey with an already-open worktree context, so a caller that holds a
/// writer lease can observe its own effect without a second census.
pub(crate) fn survey_in_context(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
    now_micros: i64,
) -> Vec<StrandedWork> {
    let Ok(trees) = worktrees::list(context, cancel) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for tree in trees {
        if tree.primary || tree.dirty != WorktreeCondition::Dirty || !unheld(&tree.occupancy) {
            continue;
        }
        let Some(branch) = tree.branch.clone() else {
            continue;
        };
        let Ok(stats) = worktrees::edit_stats(context, &tree.path, cancel) else {
            continue;
        };
        if stats.files == 0 {
            continue;
        }
        rows.push(StrandedWork {
            issue: linked_reference(&branch),
            age: stats.newest_edit_micros.map_or_else(
                || "-".to_owned(),
                |newest| age_text(now_micros.saturating_sub(newest)),
            ),
            path: tree.path,
            branch: Some(branch),
            files: stats.files,
            lines: stats.lines,
            newest_edit_micros: stats.newest_edit_micros,
        });
    }
    rows.sort_by(|left, right| left.path.cmp(&right.path));
    rows
}

/// Survey one project's worktrees. A failed inventory is reported as no rows;
/// an advisory reading never turns an observation failure into a refusal.
#[allow(clippy::too_many_arguments)]
pub(crate) fn survey(
    roots: &Roots,
    home: &Path,
    project: &Path,
    workers: &dyn worktrees::WorkerObserver,
    clock: &dyn Clock,
    cancel: &CancelToken,
    now_micros: i64,
) -> Vec<StrandedWork> {
    worktrees::with_context_using_workers(roots, home, project, clock, cancel, workers, |context| {
        Ok(survey_in_context(context, cancel, now_micros))
    })
    .unwrap_or_default()
}

/// Wall-clock microsecond reading used by callers that do not already hold it.
pub(crate) fn now_micros(clock: &dyn Clock) -> i64 {
    clock
        .wall_now()
        .map(WallTime::as_micros)
        .unwrap_or(i64::MAX)
}

// The harness-less worktree driver includes lib.rs too, where these unit
// tests are compiled but never dispatched.
#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use super::{StrandedWork, human_lines, linked_reference};
    use std::path::PathBuf;

    #[test]
    fn linked_reference_reads_a_number_from_a_branch_name() {
        assert_eq!(
            linked_reference("fix/3650-stranded-work").as_deref(),
            Some("#3650")
        );
        assert_eq!(
            linked_reference("chair/3421-ci-failures").as_deref(),
            Some("#3421")
        );
        assert_eq!(linked_reference("integration/redaction-lift"), None);
        assert_eq!(
            linked_reference("fix/2026-11-20-cleanup").as_deref(),
            Some("#2026")
        );
    }

    #[test]
    fn stranded_lines_name_counts_age_and_next_command() {
        let rows = [StrandedWork {
            path: PathBuf::from("/repo/.worktrees/fix-3650-stranded-work"),
            branch: Some("fix/3650-stranded-work".into()),
            issue: Some("#3650".into()),
            files: 1,
            lines: 3,
            age: "2d".into(),
            newest_edit_micros: Some(1),
        }];
        let lines = human_lines(&rows);
        assert_eq!(lines[0], "stranded work:");
        assert!(lines[1].contains("#3650"));
        assert!(lines[1].contains("1 file"));
        assert!(lines[1].contains("3 lines"));
        assert!(lines[1].contains("2d old"));
        assert_eq!(
            lines[2],
            "    next: provenant run implement --worktree /repo/.worktrees/fix-3650-stranded-work"
        );
    }
}
