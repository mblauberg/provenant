//! Local execution path for lane and lane_seed strategies.

use super::{ExecPlace, Job, JobOutcome, JobResult, RunContext};

/// Execute a lane job locally after the shared admission queue grants a hold.
pub fn execute(
    job: &mut Job,
    context: &RunContext,
    outcome: JobOutcome,
    waited_ms: u64,
) -> Result<JobResult, super::DispatchFailure> {
    super::local::execute(job, context, outcome, ExecPlace::Local, None, waited_ms)
}
