//! Shared classification of registered projects whose root has vanished.
//!
//! A temporary checkout can disappear while its Git common directory survives,
//! or the common directory itself can go. Neither location holds anything to
//! settle or poll, so scheduled tasks skip the binding instead of reporting a
//! fresh failure on every tick. One warning code and one stable message keep
//! the report to a single `once_per_change` warning across both tasks.

use provenant_config::ProjectBinding;
use provenant_records::warning::{Warning, detected_source, warn};

/// Whether this binding has no root left to settle or poll.
pub(crate) fn absent(binding: &ProjectBinding) -> bool {
    !binding.common_dir_present() || !binding.primary_checkout_present()
}

/// Report a vanished project root once, naming the command that unregisters it.
/// The message is stable so repeated ticks collapse onto the same dedup key.
pub(crate) fn warn_vanished(binding: &ProjectBinding) {
    let project = binding.project_id();
    warn(
        Warning::new(
            provenant_records::warning::PROJECT_ROOT_VANISHED,
            format!(
                "project {project} is no longer present at its registered root; settlement and forge polling skip it"
            ),
            detected_source(provenant_records::warning::PROJECT_ROOT_VANISHED),
        )
        .subject(project.to_string())
        .repair("run `provenant gc --apply` to unregister the vanished project")
        .once_per_change(),
    );
}
