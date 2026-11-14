# Run Git custody and delivery

Owner: `crates/confinement`, `crates/run`
Anchor: `crates/confinement/src/worktrees/owner.rs`, `crates/run/src/delivery.rs`

Test paths are relative to `tools/front-door/`. The [lane Git and import](run.md#lane-git-and-import)
contract owns private Git and publication.

## Worktrees and refs

A writer needs an explicit verified worktree. Git inventory establishes
membership; a registry reservation alone does not. Only proven-owned work is
eligible for cleanup.
- Why: custody and cleanup must not claim another checkout.
- Test: `crates/cli/src/run/worktree.rs::worktree_access_preserves_writer_and_implicit_reader_rules`

Only Provenant-created refs in their current generation may be removed. Landing
requires ancestry or a receipt chain to an authorised root; a missing commit or
rewritten target is unproven. Tree or patch equality is not landing evidence.
- Why: equivalent content does not prove that the owned work landed.
- Test: `crates/confinement/src/worktrees/lifecycle_tests/ref_ownership.rs::landed_check_uses_ancestry_and_refuses_rewritten_or_missing_commits`

Cleanup retains dirty, live and explicitly kept work, checks the expected tip and
uses a lease for deletion. Failure warns without changing settlement's outcome.
- Why: cleanup must not destroy changed or unproven work.
- Test: `crates/confinement/src/worktrees/lifecycle_tests/ref_ownership.rs::leased_ref_deletion_keeps_a_moved_or_checked_out_branch`

Sandboxed writers cannot hard-link shared files into their worktree; host seeding
copies or clones rather than hard-links shared stores.
- Why: a writable link must not mutate shared data outside the checkout.
- Test: `crates/confinement/src/permissions/tests/validation.rs::only_git_control_files_are_checked_for_other_names`

## Delivery

Dispatch checks delivery targets before admission; delivery checks protection
again. Direct push and integration refuse protected targets. Integration
conflicts do not publish a ref, and a concurrent loser records a failure for
host pickup.
- Why: publication must respect current authority and never expose a partial merge.
- Test: `crates/run/src/delivery_tests/remote_delivery.rs::local_integration_conflict_dirty_and_checked_out_refuse_without_partial_state`

Settled pickup requires a successful writer, released custody, a clean tree and
the exact recorded head. A successful pickup replays without repeating its
external effects. An existing PR is reused only for the delivered head and
expected base.
- Why: later delivery must not silently include unrelated edits.
- Test: `crates/run/src/delivery_tests/settlement.rs::settled_delivery_refuses_dirty_or_changed_head_and_replays_success`; `crates/run/src/delivery_tests/host_commands.rs::existing_pr_delivery_only_creates_after_positive_absence`

GitHub authority remains the user's. Credential selection has one host seam;
a failed mint does not fall back, and token bytes are not recorded. Project
GitHub policy is an advisory guard, not a security boundary.
- Why: a lane must not gain authority through an implicit credential fallback.
- Test: `crates/cli/src/run/provider/gh_auth.rs::selection_receipt_names_identity_source_delivery_and_mediation`
