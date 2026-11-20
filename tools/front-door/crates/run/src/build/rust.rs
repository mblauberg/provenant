//! Rust build classification seam; S1 fills the Cargo argv classifier.

use super::{Exception, Job, Priority};

/// Classification of an intercepted Rust command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RustClass {
    OffloadEligible { priority: Priority },
    Local { exception: Exception },
}

/// Classify one job for slot eligibility. S0 keeps all jobs in the fallback
/// path and lets the slot stub report PoolUnavailable.
pub fn classify(job: &Job) -> RustClass {
    RustClass::OffloadEligible {
        priority: job.priority,
    }
}
