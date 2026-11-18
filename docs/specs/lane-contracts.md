# Lane interoperability contracts

Owner: `tools/front-door/crates/records/src/lane_contract.rs`. These are the
shared contracts for subsequent admission, delivery and settlement slices; the
records alone do not implement those flows.

## Launch request

`ChildLaunchRequest` names the parent attempt, its child's narrowed authority,
and `Destination`: the parent's worktree, a managed worktree on a new or
existing branch, or a parent-owned path. Path claim and own-path commit choice
are optional, sourced and off by default. Claims are advisory unless explicitly
strict. Admission resolves paths and links and validates ownership and inherited
credentials, Git isolation, data integrity and authority; the lexical
`NarrowedAuthority::is_within` comparison is only one check, not a grant. Routes
are a subset, budgets cannot exceed or drop a parent ceiling, and paths must be
under an allowed root. Unknown authority fields make the comparison inconclusive
(and therefore false) until their semantics are understood. Other inherited
boundary restrictions cannot widen.

## Observation events

`ObservationEvent` is versioned, has a stable deduplication identity, project
sequence cursor, lineage, kind and coverage. Delivery is at least once; consumers
persist a cursor after processing and deduplicate by identity. Order is within a
project stream, not across projects. Forge events may have no lane attempt;
normalised detail identifies repository, PR, head and check where available.
Unknown and partial coverage remain explicit.

The delivery slice appends the shared stream to the existing project event store
(`fabric-store/src/events.rs`) and uses its replay and per-consumer cursor
pattern. Lane lifecycle comes from attempt settlement and the existing graph
`*.events.jsonl` journal; notes retain their append-only state files; forge
check_run, check_suite, workflow_run, pull_request and issue_comment originate
at the host listener. Those source stores remain authoritative; the shared
stream indexes observations for replay and does not replace them. The store's
`append_observation` deduplicates stable source identities and `observations`
assigns the project cursor on replay. `provenant events --from SEQ [--follow]`
emits those observations as JSON lines; readers persist a cursor only after
processing. Source ingestion, listener and chair adapter remain separate work.

## Result projection

`ResultProjection` presents bounded `HostFacts` first and `LaneClaims` second.
Observed commands carry source and completeness, with unknown exit codes left
unknown. Git, CI and scope evidence likewise state source and completeness;
shared-worktree Git facts describe the worktree, not child authorship. The
lane's handoff and verify are claims, not host attestations. The host builds the
projection and bounds its encoded input to the existing run result limit;
truncation and missing evidence must be reported as partial or unknown. Status,
output, resume, steer and provider adapters consume this same projection in the
later facts slice.
