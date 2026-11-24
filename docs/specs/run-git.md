# Run Git custody and delivery

Owner: `crates/confinement`, `crates/run`
Anchor: `crates/confinement/src/worktrees/owner.rs`, `crates/run/src/delivery.rs`

Paths are relative to `tools/front-door/`. The [lane Git and import](run.md#lane-git-and-import)
contract owns private Git and publication.

## Worktrees and refs

### Project layout ownership

Project identity is keyed by the canonical Git common directory. A project-scoped path is owned by
the primary checkout proved by Git's worktree inventory, or by a selected checkout whose private
and common Git directories match. When an external Git directory keeps inventory from naming
that checkout, a saved path fills the gap only if it was captured from a verified primary and Git
re-proves it is still the primary for the same common directory. Linked-worktree membership alone
never proves primary ownership. A bare repository has no project checkout and keeps host and
common-only layout, and the common directory's parent is never a checkout claim.
- Why: external metadata directories and linked checkouts do not encode a primary path by parent
  relationship.

A writer needs an explicit verified worktree: Git inventory establishes membership, a registry
reservation alone does not, and only proven-owned work is eligible for cleanup.
- Why: custody and cleanup must not claim another checkout.

Only Provenant-created refs in their current generation may be removed. Landing requires ancestry
or a receipt chain to an authorised root; a missing commit or rewritten target is unproven, and
tree or patch equality is not landing evidence.
- Why: equivalent content does not prove that the owned work landed.

Cleanup retains dirty, live and explicitly kept work, checks the expected tip and uses a lease for
deletion. A failure warns without changing settlement's outcome.
- Why: cleanup must not destroy changed or unproven work.

Host seeding copies or clones shared stores rather than hard-linking them; lane hard links follow
the [permission policy](supervisor.md#permission-policy).
- Why: a writable link must not mutate shared data outside the checkout.

## One resolved forge context per command

Exactly one `ForgeContext` is resolved by the owner that needs a forge or a remote, then passed
down; no consumer rebuilds it. It carries the remote, default branch, protected set and operation
policy, each with the layer and key that produced the value.
- Why: three independent discoveries of `origin`/`main` could disagree within one command, and a
  policy resolved by the owner was replaced by a compiled default at the forge port.

The remote is the declared `git.remote`, else the current branch's upstream remote, else the sole
configured remote; a tracking-ref directory under `refs/remotes` is not a remote unless no remote is
configured at all, as in a bare store fed by explicit fetches. A refusal lists the remotes seen with close matches, never an assumed `origin`. The default
branch is the declared `git.default_branch`, else `refs/remotes/<remote>/HEAD`, else the forge API;
protected branches are declared, else the default branch plus forge-reported protection.
- Why: a hidden guess can retire the wrong branch or push to the wrong repository.

A new branch name collides with an existing ref when their NFC-lowercased components match or one is
a path prefix of the other. A remote-tracking ref is split into a remote and a branch only at a
configured remote name, longest name first; an intermediate slash never invents a remote, but a
configured remote that contains `/` is still a valid boundary.
- Why: treating every slash as a remote boundary invents remotes that do not exist and refuses names
  that collide with nothing.

The owner resolves the context when a delivery or target check first needs it (`ForgeOnce`), so a
command that delivers nothing, such as a read-only graph, runs in a checkout with no remote.
- Why: an eager resolution refused read-only work for a remote it never used.

## Delivery

Under `host` access, `HostDeliveryContext` accepts host-configured Git transport helpers and URL
features with a warning; `isolated` refuses them. Transport checks still protect configuration a
sandboxed lane can write, and credential-bearing URLs stay out of process arguments. Delivery
bindings, receipts and the ref ledger keep only the URL without userinfo, and cleanup re-resolves
the push URL and checks its credential-free identity against the recorded endpoint.

Dispatch checks delivery targets before admission and delivery checks protection again. Direct
push and integration refuse protected targets. Integration conflicts publish no ref, and a
concurrent loser records a failure for host pickup.
- Why: publication must respect current authority and never expose a partial merge.

Settled pickup requires a successful writer, released custody, a clean tree and the exact recorded
head. A successful pickup replays without repeating its external effects, and an existing PR is
reused only for the delivered head and expected base.
- Why: later delivery must not silently include unrelated edits.

GitHub authority stays the user's. Credential selection has one host seam, a failed mint does not
fall back, and token bytes are never recorded. Project GitHub policy is an advisory guard, not a
security boundary.
- Why: a lane must not gain authority through an implicit credential fallback.
