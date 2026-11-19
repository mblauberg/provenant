# Run launch and admission

Owner: `crates/run`, `crates/confinement`, `crates/providers`
Anchor: `crates/run/src/launch.rs`, `crates/run/src/admission.rs`

Paths are relative to `tools/front-door/`.

## Admission and custody

Admission commits the reservation and attempt together. A verified worktree has
at most one writer; resource probes do not hold the shared registry lock or run
inside the transaction. Busy capacity is not a queue. Direct-run custody uses
the same captured project and actor identity as the run store. Uncertain custody
keeps its ownership until release is proved. Writer-bind refusals identify the durable lease holder, branch, worktree, reservation age and observed liveness. Reservation age is measured from its last durable update. Reconciliation reports released custody only after exclusion and native absence evidence prove it safe; an unknown or live holder retains custody. Uncertain custody and unreadable reservations are reported as warnings while reconciliation continues; they remain fenced until safe release is proved. Stable lock inodes remain in place so concurrent observers cannot split exclusion.
- Why: admission must not create duplicate writers or hold coordination locks across external work.

Before entering handoff or spawning a guardian, the launcher durably prepares
the per-attempt `guardian.lock` inode alongside the attempt, writer and custody
identities. The guardian acquires and verifies that original inode before
acknowledging custody or spawning the provider, then records its native process
identity. The prepared record is sufficient even if the guardian dies before
publishing its own identity or updating the pre-handoff reservation: the
original free writer and custody leases exclude a live launcher or guardian.
Only the guardian holds the life lock after preparation;
the provider and its descendants never inherit it. The guardian retains it
through terminal publication and custody release. Kernel process teardown
releases it even when the guardian cannot publish an exit.
Tick's settlement driver consumes that proof under writer exclusion and a
fresh worktree occupancy census. A retained or unknown process observation
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

A pre-launch routing refusal retains a bounded host-authored explanation in
its result alongside the safe cause and remedy. Provider-originated diagnostics
never supply that public explanation.

Terminal attempts are immutable. A retry is a new linked attempt, and a writer
is never replayed over its edits. A terminal attempt has at most one successor.
- Why: continuation must preserve both history and exclusive custody.

## Graph naming

A named launch treats its name as a handle for the launch input. The same
input replays the existing graph and starts nothing. Changed input under that
name starts the next free `NAME-k` (`NAME-2`, `NAME-3`, ...) and leaves earlier
graphs alone; an unnamed launch takes the next `BASE-k` from 1 and never
replays. A name is one lowercase letter, then lowercase letters, digits and
single hyphens, ending in a letter or digit. A grammar refusal suggests the
normalised form, so `--name Foo-` is refused with `try \`foo\``.
- Why: a reused name must not silently attach a different brief to old work.

A suffixed launch says so in its first output line:
`started NAME-2 (NAME exists with a different input)`. The `--json` receipt
carries the prior handle as `named.superseded` alongside `named.graph`.
`wait NAME` and `status NAME` keep reading the older graph, but warn that it
was superseded and print the newer handle as the next step
(`next: provenant wait NAME-2`). They never redirect silently.
- Why: the operator must see the new handle without losing access to the old graph.

## Writer binding

`--branch B --base REF` allocates a managed writer worktree under the project
layout, not under the invoking checkout. The launch owner resolves and
checkpoints `REF` before any Git effect, then creates the worktree from that
commit and binds it by source identity and head. This is identical from the
primary checkout, a linked worktree, a separate Git directory or a bare linked
checkout: the project's saved primary layout owns the allocation in every case.
An existing clean worktree at the exact checkpointed head is rebound; a
different branch, head or unclean tree is refused with the named cause.
- Why: an owned writer must land in one project-owned location regardless of
  where the launch command ran, and a failed bind must not leave a half-made
  worktree.

A refused bind keeps its typed cause all the way to the facade. A held writer
lease is `writer_lease_held` and names its holder locally; scope, confinement
and resource failures keep their own codes and repair actions instead of
reporting `confinement_refused` with no resource. See
`docs/specs/cli.md#run-refusals-keep-their-own-code-at-the-facade`.
- Why: the operator needs the real cause to repair a launch.

## Policy and confinement

Launch policy refuses only at the [hard boundaries](../adr/0035-three-hard-boundaries-and-flexible-everything-else.md).
Other checks allow or warn unless configured otherwise. A write-scope refusal happens before publication and retains private
work. The lane instructions say the same: a write scope lists the expected files,
so a writer edits another file when the fix needs it and lists it in its
handoff; only a strict `WRITE_SCOPE` policy refuses it.
- Why: advisory checks must not become accidental authority boundaries.

Policy rows live in `crates/run/assets/policy.toml`. Non-boundary rules default
to warning or off; a rule with a meaningful stop path may offer an explicit
`refuse` setting, which produces a configured stop. `Policy::warn_or` reports the source and knob, while immutable
boundary refusals use `boundary_error`. Heuristic scanners cannot offer a stop.
The `policy.<CODE>` knobs are documented in `config/harness-knobs.json`.

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
