//! Registry contract for independently budgeted scheduled tasks.

use super::TaskResult;

/// A tick task owns its worker execution and returns the same durable result
/// shape used by the existing scheduler.
#[allow(dead_code)]
pub(super) trait TickTask: Send + Sync {
    fn name(&self) -> &'static str;
    fn run(&self) -> TaskResult;
}
