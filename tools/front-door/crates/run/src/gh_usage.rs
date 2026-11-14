//! Settlement's record of a lane's `gh` invocations. The lane's gh wrapper
//! appends one byte per call to a file in its scratch directory; the size is
//! the count. The lane can write that file, so the count is advisory: it is
//! read without following links, bounded, and never gates anything.
use provenant_os::fs::{Directory, FileKind};
use provenant_records::{LaneLimits, run::UsageRecord};

/// Candidate scratch directories scanned under the attempt's `tmp`.
const CANDIDATES: usize = 32;

/// Record the lane's gh invocation count, its budget and the budget's source
/// in `usage`. Nothing is recorded when no wrapper ran.
pub(crate) fn record(attempt: &Directory, limits: Option<&LaneLimits>, usage: &mut UsageRecord) {
    let Some(count) = count(attempt) else {
        return;
    };
    let budget = limits.and_then(|limits| limits.gh_invocations.as_ref());
    usage.extra.insert(
        "gh_invocations".into(),
        serde_json::json!({
            "count": count,
            "budget": budget.map(|limit| limit.value),
            "source": budget.map(|limit| limit.source.as_str()),
            "over_budget": budget.is_some_and(|limit| count >= limit.value),
        }),
    );
}

fn count(attempt: &Directory) -> Option<u64> {
    let temporary = attempt.open_directory("tmp".as_ref()).ok()?;
    let name = std::ffi::OsStr::new(provenant_config::GH_INVOCATIONS_FILE);
    let counts: Vec<u64> = temporary
        .entries()
        .ok()?
        .map_while(Result::ok)
        .take(CANDIDATES)
        .filter_map(|candidate| temporary.open_directory(&candidate).ok())
        .filter_map(|scratch| scratch.metadata_at(name, false).ok())
        .filter(|metadata| metadata.kind == FileKind::Regular)
        .map(|metadata| metadata.size)
        .collect();
    (!counts.is_empty()).then(|| counts.into_iter().sum())
}
