//! Typed permission plans and the fail-closed macOS SBPL backend.
//!
//! Provider-owned profile data bounds every requested permission.
//! Linux deliberately exposes an unavailable backend.

mod anchors;
mod grants;
mod policy;
mod probe;
#[cfg(any(target_os = "macos", feature = "snapshot"))]
mod render;
mod types;
mod validation;

pub use provenant_records::run::{NetworkAccess, PathScope, WriteSource};
#[cfg(feature = "snapshot")]
use std::path::Path;

pub use self::anchors::*;
pub use self::grants::*;
pub use self::policy::*;
pub use self::probe::*;
#[cfg(any(target_os = "macos", feature = "snapshot"))]
use self::render::*;
pub use self::types::*;
pub use self::validation::*;

#[cfg(feature = "snapshot")]
pub mod snapshot;

#[cfg(test)]
mod tests {
    use super::*;
    // Shared by the native-only test modules below.
    #[cfg(target_os = "macos")]
    use self::grants::{worktree_grants_fixture, write_grants_fixture};
    #[cfg(target_os = "macos")]
    use provenant_core::CancelToken;
    #[cfg(target_os = "macos")]
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor};
    #[cfg(target_os = "macos")]
    use std::collections::{BTreeMap, BTreeSet};
    #[cfg(target_os = "macos")]
    use std::ffi::OsString;
    #[cfg(target_os = "macos")]
    use std::path::{Path, PathBuf};
    #[cfg(target_os = "macos")]
    use std::time::Duration;

    #[cfg(target_os = "macos")]
    mod escape;
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn native_sandbox_blocks_path_escape_probes() -> Result<(), String> {
        provenant_test_support::skip_sandbox_dependent_test(
            "native Seatbelt lock contention requires macOS",
        )
    }
    mod grants;
    mod host_boundaries;
    mod ipc;
    mod mach_lookup;
    mod process_info;
    mod process_tree;
    mod render;
    mod signals;
    mod sysctl;
    mod sysv_audit;
    mod validation;
}
