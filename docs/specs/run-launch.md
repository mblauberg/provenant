# Run launch and admission

Owner: `crates/run`, `crates/confinement`, `crates/providers`
Anchor: `crates/run/src/launch.rs`, `crates/run/src/admission.rs`

Paths are relative to `tools/front-door/`.

## Admission and custody

Admission commits the reservation and attempt together. A worktree's owning lane
decides who else writes in it; active lanes share an occupancy lease. Resource
probes run outside the admission transaction. Busy capacity is not a queue. Direct-run custody uses
the same captured project and actor identity as the run store. Uncertain custody
keeps its ownership until release is proved.
- Why: shared work must remain possible without holding coordination locks across external work.

Nested sharing is authorised by the owning guardian's host-sealed worktree
incarnation and a kernel-observed caller relationship. An existing nested writer
may share that exact checkout; independent work provisions its own checkout.
Admission pins the shared checkout's directory identity, rechecked at dispatch
and after acquiring occupancy, so a removed/recreated path cannot inherit the
grant. Nested provisioning records its creating graph/node in the checkout's
private Git directory; recovery never reclaims an existing checkout without
that receipt. Missing creation evidence retains the checkout for host recovery.
Shared launches observe the live branch and never advance the parent's
checkout. Direct host requests retain authority to select existing checkouts.
Optional `write_scope` paths are advisory claims: overlaps in one worktree warn
the owning lane in its admission receipt and checkpoint and refuse nothing.
Writers use pathspec commits (`git commit -- <owned paths>`) to avoid committing
a sibling's staged paths.
- Why: sharing follows the owner's decision while unrelated lanes stay isolated.

Authorised writers share incarnation- and branch-bound private Git data,
including HEAD, index, refs and objects. Per-attempt configuration and credentials
remain separate; private data outlives an individual attempt's scratch.
Git owns the atomicity of config, index, refs and worktree registration. Provenant
holds no repository-wide lock around Git commands. Explicit `*.lock` collisions
receive brief bounded retries with jitter; semantic conflicts and permission
failures are returned immediately. Prepared ref deletion retries only before
Git has acknowledged preparation. A satisfied source pin has no checkout effect
and does not exclude shared holders or their edits; an actual checkout advance
still requires exclusive occupancy.
- Why: independent worktrees and coherent config reads must not queue behind sibling launches.

Before entering handoff or spawning a guardian, the launcher durably prepares
the per-attempt `guardian.lock` inode alongside the attempt, writer and custody
identities. The guardian acquires and verifies that original inode before
acknowledging custody or spawning the provider, then records its native process
identity. The prepared record is sufficient even if the guardian dies before
publishing its own identity or updating the pre-handoff reservation: the
original free holder and custody leases exclude a live launcher or guardian.
Only the guardian holds the life lock after preparation;
the provider and its descendants never inherit it. The guardian retains it
through terminal publication and custody release. Kernel process teardown
releases it even when the guardian cannot publish an exit.
Tick's settlement driver consumes that proof under the appropriate occupancy
lease and a fresh worktree occupancy census. A retained or unknown process observation
cannot substitute for the original free locks or positive non-occupancy.
- Why: a stale process checkpoint cannot prove same-boot guardian death; the
  original kernel lock can supply that evidence without PID-reuse heuristics.

A launch is uncertain only once its reservation may record a handoff. A host
that cannot hold provider custody refuses before that point: the attempt ends
with `unsupported_platform` and its custody is withdrawn, never reported as an
unknown launch awaiting a receipt. A launch cancelled or refused before it
spawns needs no custody, so it records `never_spawned` the same way on every
host.
- Why: a launch that cannot start must not look like a lost lane.

Permanent pre-admission refusals are durable outcomes of their operation. Replay
returns the refusal without preparing again; a failure to store it is not
acknowledged as success.
- Why: a lost response must not turn a refusal into another launch.

Terminal attempts are immutable. A retry is a new linked attempt, and a writer
is never replayed over its edits. A terminal attempt has at most one successor.
- Why: continuation must preserve both history and its attempt's custody.

## Policy and confinement

Launch policy defaults to refusal only at credentials, Git isolation, data
integrity and authority boundaries. Other checks allow or warn unless configured
otherwise. A write-scope refusal happens before publication and retains private
work. The lane instructions say the same: a write scope lists the expected files,
so a writer edits another file when the fix needs it and lists it in its
handoff; only a strict `WRITE_SCOPE` policy refuses it.
- Why: advisory checks must not become accidental authority boundaries.

Every non-boundary rule lives in `crates/run/assets/policy.toml`, defaults to
`warn` and is `refusable`, so a project can opt into strict mode. Its knob
`policy.<CODE>` is documented under `policy_rules` in
`config/harness-knobs.json`. A consumer calls `Policy::warn_or`, which warns on
stderr naming the rule, the setting source and the knob, or returns the
caller's refusal under strict mode. Boundary rules keep using `boundary_error`.

Every provider lane has its native tool and shell surface. Read-only mode
constrains filesystem writes; it does not remove shell, editing or web tools
from the provider. The common confinement planner owns worktree writes, reads
of the primary and sibling checkouts, explicit read roots, credential denials
and network reach. Network defaults to open, and explicit network reach is
applied by that same planner for every adapter. Every launch finalises the
host Cargo home and lane target binding through the common environment path.
Private harness instruction files receive the canonical global rules compiled
into the binary, so source checkouts and packaged releases use identical content
without depending on the installed asset layout. Native integration differences
belong to `AdapterSpec`, with a reason and source for an unavailable capability.
- Why: provider selection must not silently narrow a lane's working surface.

The selected sandbox floor applies to each candidate and is sealed for
continuations. Unsupported confinement is reported rather than represented as
sandboxed execution.
- Why: a retry must not silently weaken the requested boundary.

Read-only write exceptions come from user-owned configuration, remain within
verified repository paths and are recorded on the attempt. Git and provider
control paths are excluded.
- Why: only the user may widen a read-only lane's write authority.

Host-native launches retain confinement and provenance and cannot resume a
provider session or select model-specific controls.
- Why: native execution must not claim a provider contract it cannot satisfy.

## Inputs and resources

Unreadable required host inputs reject an attempt before launch. A pinned source
view reports its failure without exposing captured contents or credentials.
- Why: a partial source view must not be mistaken for the requested input.

Lane download stores do not copy the host registry, and fresh provider homes do
not carry provider history.
- Why: shared build inputs must not disclose host or provider state.

Build output is collected only when its Cargo locks are free and recent writes
are protected. Priority adjustment failure does not refuse launch.
- Why: resource management must not corrupt active work or prevent a launch.
