//! Bounded, advisory counts of uncommitted work in a linked worktree.
//!
//! The numbers here only inform reports such as stranded work. They never
//! grant or refuse removal authority; the ordinary cleanliness verdict still
//! decides whether a checkout may be collected.
use super::*;
use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt;
use std::time::UNIX_EPOCH;

/// One worktree's uncommitted edits, counted for reporting only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct EditStats {
    /// Distinct non-ignored paths reported by Git.
    pub files: usize,
    /// Added or removed lines, plus lines in untracked files.
    pub lines: usize,
    /// Newest modification time among the changed files still on disk.
    pub newest_edit_micros: Option<i64>,
}

const MAX_UNTRACKED_BYTES: u64 = 1_048_576;
const MAX_NUMSTAT_BYTES: usize = 1_048_576;
const MAX_NUMSTAT_ROWS: usize = 10_000;

/// Count added and removed lines from `git diff --numstat -z`. Numeric counts
/// never depend on patch line prefixes, so a changed line beginning `++` or
/// `--` is counted like any other. Binary entries report `-` and add nothing.
fn count_numstat_lines(raw: &[u8]) -> Result<usize, WorktreeError> {
    if raw.len() > MAX_NUMSTAT_BYTES || (!raw.is_empty() && !raw.ends_with(&[0])) {
        return Err(WorktreeError::InvalidOutput);
    }
    let mut total = 0usize;
    let mut rows = 0usize;
    for record in raw.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        rows += 1;
        if rows > MAX_NUMSTAT_ROWS {
            return Err(WorktreeError::InvalidOutput);
        }
        let mut fields = record.splitn(3, |byte| *byte == b'\t');
        let added = fields.next().ok_or(WorktreeError::InvalidOutput)?;
        let removed = fields.next().ok_or(WorktreeError::InvalidOutput)?;
        if fields.next().is_none() {
            return Err(WorktreeError::InvalidOutput);
        }
        for value in [added, removed] {
            if value == b"-" {
                continue;
            }
            let text = std::str::from_utf8(value).map_err(|_| WorktreeError::InvalidOutput)?;
            total = total.saturating_add(
                text.parse::<usize>()
                    .map_err(|_| WorktreeError::InvalidOutput)?,
            );
        }
    }
    Ok(total)
}

fn modified_micros(metadata: &std::fs::Metadata) -> Option<i64> {
    let elapsed = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(elapsed.as_micros().min(i64::MAX as u128) as i64)
}

fn count_lines(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    bytes.iter().filter(|byte| **byte == b'\n').count() + usize::from(!bytes.ends_with(b"\n"))
}

/// Count changed files, changed lines and the newest edit in one worktree.
/// Every Git read is bounded and read-only. A failed read reports no stats
/// rather than turning an uncertain observation into a firm count.
pub fn edit_stats(
    context: &WorktreeContext<'_>,
    path: &Path,
    cancel: &CancelToken,
) -> Result<EditStats, WorktreeError> {
    let tree = super::custody::verify_probed_worktree(context, path, cancel)?;
    let status = super::inventory::run_git(
        context,
        tree.repo(),
        GitRead::StatusIncludingIgnored,
        cancel,
    )?;
    if super::inventory::exit_code(&status)? != 0 {
        return Err(WorktreeError::InvalidOutput);
    }
    let records =
        parse_porcelain_v2_z(&status.stdout.bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    let mut paths = BTreeSet::new();
    let mut untracked = BTreeSet::new();
    for record in records {
        if record.kind == StatusKind::Ignored {
            continue;
        }
        if record.kind == StatusKind::Untracked {
            untracked.insert(record.path.clone());
        }
        paths.insert(record.path);
    }
    let files = paths.len();

    let mut stats = EditStats {
        files,
        ..EditStats::default()
    };
    if files == 0 {
        return Ok(stats);
    }

    let numstat = super::inventory::run_git(context, tree.repo(), GitRead::Numstat, cancel)?;
    if super::inventory::exit_code(&numstat)? == 0
        && !numstat.drain_truncated
        && !numstat.stdout.sink_failed
        && !numstat.stdout.read_failed
    {
        stats.lines = count_numstat_lines(&numstat.stdout.bytes)?;
    }

    for raw in &paths {
        let relative = Path::new(std::ffi::OsStr::from_bytes(raw));
        if !relative
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            continue;
        }
        let absolute = path.join(relative);
        let Ok(metadata) = std::fs::symlink_metadata(&absolute) else {
            continue;
        };
        if let Some(modified) = modified_micros(&metadata) {
            stats.newest_edit_micros = Some(
                stats
                    .newest_edit_micros
                    .map_or(modified, |newest| newest.max(modified)),
            );
        }
        // A diff against HEAD does not include untracked files. Count their
        // lines from the file itself, within a fixed read bound.
        if untracked.contains(raw)
            && metadata.is_file()
            && metadata.len() <= MAX_UNTRACKED_BYTES
            && let Ok(bytes) = std::fs::read(&absolute)
        {
            stats.lines += count_lines(&bytes);
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::{WorktreeError, count_numstat_lines};

    #[test]
    fn numstat_counts_added_and_removed_lines_from_numbers_not_patch_prefixes() {
        // The patch for `++added` / `--removed` renders as `+++` / `---`;
        // numstat reports only the counts, so both lines are counted.
        assert_eq!(count_numstat_lines(b"1\t1\tweird.rs\0").unwrap(), 2);
        assert_eq!(
            count_numstat_lines(b"5\t0\tadd.rs\0-\t-\timage.png\0").unwrap(),
            5
        );
        assert_eq!(count_numstat_lines(b"").unwrap(), 0);
    }

    #[test]
    fn numstat_rejects_malformed_or_unbounded_output() {
        assert!(matches!(
            count_numstat_lines(b"1\t1\tunterminated"),
            Err(WorktreeError::InvalidOutput)
        ));
        assert!(matches!(
            count_numstat_lines(b"1\tfile.rs\0"),
            Err(WorktreeError::InvalidOutput)
        ));
        assert!(matches!(
            count_numstat_lines(b"bad\t1\tfile.rs\0"),
            Err(WorktreeError::InvalidOutput)
        ));
    }
}
