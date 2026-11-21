//! Settlement's record of a lane's Git hook-bypass warnings. The bound Git
//! shim appends one line per bypass to a file in its scratch directory. The
//! lane can write that file, so the observation is advisory: the host reads it
//! without following links, bounded, and never gates anything. It is recorded
//! in `usage`, which the handoff and result surface to the chair.
use provenant_records::run::{HookBypassReceipt, LANE_HOOK_BYPASS_RULE, UsageRecord};

/// Candidate scratch directories scanned under the attempt's `tmp`.
const CANDIDATES: usize = 32;
/// One warning line; a bounded read keeps a lane-written file from growing cost.
const MAX_BYTES: u64 = 4096;

/// Record the shim's hook-bypass count in `usage`. Nothing is recorded when no
/// lane-bound shim warned.
pub(crate) fn record(attempt: &provenant_os::fs::Directory, usage: &mut UsageRecord) {
    let Some(count) = count(attempt) else {
        return;
    };
    usage.hook_bypass = Some(HookBypassReceipt {
        count,
        rule: LANE_HOOK_BYPASS_RULE.into(),
        extra: Default::default(),
    });
}

fn count(attempt: &provenant_os::fs::Directory) -> Option<u64> {
    let temporary = attempt.open_directory("tmp".as_ref()).ok()?;
    let name = std::ffi::OsStr::new(provenant_config::HOOK_OBSERVATIONS_FILE);
    let counts: Vec<u64> = temporary
        .entries()
        .ok()?
        .map_while(Result::ok)
        .take(CANDIDATES)
        .filter_map(|candidate| temporary.open_directory(&candidate).ok())
        .filter_map(|scratch| scratch.read_bounded(name, MAX_BYTES).ok())
        .map(|bytes| bytes.iter().filter(|&&byte| byte == b'\n').count() as u64)
        .collect();
    (!counts.is_empty()).then(|| counts.into_iter().sum())
}

#[cfg(test)]
mod tests {
    #[test]
    fn no_observations_record_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let attempt = provenant_os::fs::Directory::open_anchor(dir.path()).unwrap();
        let mut usage = provenant_records::run::UsageRecord::default();
        super::record(&attempt, &mut usage);
        assert_eq!(usage.hook_bypass, None);
    }

    #[test]
    fn bounded_observations_are_counted_into_usage() {
        let dir = tempfile::tempdir().unwrap();
        let attempt = provenant_os::fs::Directory::open_anchor(dir.path()).unwrap();
        attempt.mkdir("tmp".as_ref(), 0o700).unwrap();
        let temporary = attempt.open_directory("tmp".as_ref()).unwrap();
        temporary.mkdir("candidate-0".as_ref(), 0o700).unwrap();
        let scratch = temporary.open_directory("candidate-0".as_ref()).unwrap();
        scratch
            .atomic_write(
                provenant_config::HOOK_OBSERVATIONS_FILE.as_ref(),
                ".obs-",
                b"LANE_GIT_HOOK_BYPASS\nLANE_GIT_HOOK_BYPASS\n",
                provenant_os::fs::PublishMode::NoReplace,
            )
            .unwrap();
        let mut usage = provenant_records::run::UsageRecord::default();
        super::record(&attempt, &mut usage);
        let receipt = usage.hook_bypass.expect("hook bypass observation");
        assert_eq!(receipt.count, 2);
        assert_eq!(receipt.rule, "LANE_GIT_HOOK_BYPASS");
    }
}
