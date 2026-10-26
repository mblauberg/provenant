//! Seam for byte-for-byte tests of the rendered Seatbelt profile.
//!
//! Compiled only with the `snapshot` feature, on any platform. It renders what
//! `compile_sbpl` would for a plan and profile, without anchoring or reading
//! the filesystem, and gathers grants through the same `extend_read_grants`.

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
    let worktree_grants = matches!(plan.mode, AccessMode::WorktreeWrite(_)).then(|| {
        WorktreeWriteGrants::from_paths(
            Path::new(WORKTREE),
            Path::new(PRIVATE_GIT_DIR),
            Path::new(COMMON_GIT_DIR),
            BRANCH,
        )
    });
    let mut read_grants = plan.read_roots.clone();
    if let Some(read_only) = &plan.read_only {
        read_grants.extend(read_only.git_reads.iter().cloned());
    }
    let credential_writes =
        extend_read_grants(&mut read_grants, plan, profile, worktree_grants.as_ref())?;
    let bytes = render_profile(
        plan,
        profile,
        &read_grants,
        &credential_writes,
        worktree_grants.as_ref(),
        home,
    )?;
    String::from_utf8(bytes).map_err(|_| PermissionError::NonUtf8Path)
}
