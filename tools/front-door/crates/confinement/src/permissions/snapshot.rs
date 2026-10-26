//! Seam for byte-for-byte tests of the rendered Seatbelt profile.
//!
//! Compiled only with the `snapshot` feature, on any platform. It renders what
//! `compile_sbpl` would for a plan and profile, without anchoring or reading
//! the filesystem, and gathers grants through the same `GrantSet::build`.
//! On macOS it also compiles against held anchors without the sandbox probe.

use super::*;

const WORKTREE: &str = "/fixture/worktrees/lane";
const PRIVATE_GIT_DIR: &str = "/fixture/repo/.git/worktrees/lane";
const COMMON_GIT_DIR: &str = "/fixture/repo/.git";
const BRANCH: &str = "lane";

/// The profile text for `plan` and `profile`. A writer plan gets fixed
/// worktree and Git grants, laid out by the production derivation.
pub fn render(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    home: &Path,
) -> Result<String, PermissionError> {
    let worktree = matches!(plan.mode, AccessMode::WorktreeWrite(_)).then(|| {
        WorktreeWriteGrants::from_paths(
            Path::new(WORKTREE),
            Path::new(PRIVATE_GIT_DIR),
            Path::new(COMMON_GIT_DIR),
            BRANCH,
        )
    });
    let grants = GrantSet::build(plan, profile, home, worktree)?;
    super::render(&grants)
}

/// What the probed backend's `compile` returns for `plan` and `profile`
/// against held anchors. Tests cannot rely on the probe, which a process that
/// is already sandboxed cannot run.
#[cfg(target_os = "macos")]
pub fn compile(
    plan: &PlanPermissions,
    profile: &ProviderProfile,
    anchors: &Anchors<'_>,
) -> Result<ConfinedLaunch, PermissionError> {
    compile_sbpl(plan, profile, anchors)
}
