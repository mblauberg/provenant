# Run Git custody and delivery

Owner: `crates/confinement`, `crates/run`
Anchor: `crates/confinement/src/worktrees/owner.rs`, `crates/run/src/delivery.rs`

Paths are relative to `tools/front-door/`. The [lane Git and import](run.md#lane-git-and-import)
contract owns private Git and publication.

## Worktrees and refs

### Project layout ownership

Project identity remains keyed by the canonical Git common directory. A
project-scoped path is owned by the primary checkout proved by Git's worktree
inventory or by a selected checkout whose private and common Git directories
match. When an external Git directory prevents inventory from naming that
checkout, a saved path may fill the gap only if it was captured from a verified
primary and Git re-proves that it is still the primary for the same common
directory. Linked-worktree membership alone does not prove primary ownership.
A bare repository has no project checkout and keeps host/common-only layout.
The common-directory parent is never a checkout claim.
- Why: external metadata directories and linked checkouts do not encode a
  primary path by parent relationship.

A writer needs an explicit verified worktree. Git inventory establishes
membership; a registry reservation alone does not. Only proven-owned work is
eligible for cleanup.
- Why: custody and cleanup must not claim another checkout.

Only Provenant-created refs in their current generation may be removed. Landing
requires ancestry or a receipt chain to an authorised root; a missing commit or
rewritten target is unproven. Tree or patch equality is not landing evidence.
- Why: equivalent content does not prove that the owned work landed.

Cleanup retains dirty, live and explicitly kept work, checks the expected tip and
uses a lease for deletion. Failure warns without changing settlement's outcome.
- Why: cleanup must not destroy changed or unproven work.

Sandboxed writers cannot hard-link shared files into their worktree; host seeding
copies or clones rather than hard-links shared stores.
- Why: a writable link must not mutate shared data outside the checkout.

## Delivery

Dispatch checks delivery targets before admission; delivery checks protection
again. Direct push and integration refuse protected targets. Integration
conflicts do not publish a ref, and a concurrent loser records a failure for
host pickup.
- Why: publication must respect current authority and never expose a partial merge.

Settled pickup requires a successful writer, released custody, a clean tree and
the exact recorded head. A successful pickup replays without repeating its
external effects. An existing PR is reused only for the delivered head and
expected base.
- Why: later delivery must not silently include unrelated edits.

GitHub authority remains the user's. Credential selection has one host seam;
a failed mint does not fall back, and token bytes are not recorded. Project
GitHub policy is an advisory guard, not a security boundary.
- Why: a lane must not gain authority through an implicit credential fallback.
