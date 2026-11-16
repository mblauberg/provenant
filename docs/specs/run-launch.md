# Run launch and admission

Owner: `crates/run`, `crates/confinement`, `crates/providers`
Anchor: `crates/run/src/launch.rs`, `crates/run/src/admission.rs`

Paths are relative to `tools/front-door/`.

## Admission and custody

Admission commits the reservation and attempt together. A verified worktree has
at most one writer; resource probes do not hold the shared registry lock or run
inside the transaction. Busy capacity is not a queue. Direct-run custody uses
the same captured project and actor identity as the run store. Uncertain custody
keeps its ownership until release is proved.
- Why: admission must not create duplicate writers or hold coordination locks across external work.

Permanent pre-admission refusals are durable outcomes of their operation. Replay
returns the refusal without preparing again; a failure to store it is not
acknowledged as success.
- Why: a lost response must not turn a refusal into another launch.

Terminal attempts are immutable. A retry is a new linked attempt, and a writer
is never replayed over its edits. A terminal attempt has at most one successor.
- Why: continuation must preserve both history and exclusive custody.

## Policy and confinement

Launch policy defaults to refusal only at credentials, Git isolation, data
integrity and authority boundaries. Other checks allow or warn unless configured
otherwise. A write-scope refusal happens before publication and retains private
work.
- Why: advisory checks must not become accidental authority boundaries.

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
