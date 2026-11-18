# Lane interoperability contracts

Owner: `tools/front-door/crates/records/src/lane_contract.rs`
Anchor: `tools/front-door/crates/records/src/lane_contract.rs`,
`tools/front-door/crates/fabric-runtime/src/notes.rs`

These are the shared contracts for subsequent admission, delivery and settlement slices; the
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
(`tools/front-door/crates/fabric-store/src/events.rs`) and uses its replay and per-consumer cursor
pattern. Lane lifecycle comes from attempt settlement and the existing graph
`*.events.jsonl` journal; notes retain their append-only state files; forge
check_run, check_suite, workflow_run, pull_request and issue_comment originate
at the host listener. Those source stores remain authoritative; the shared
stream indexes observations for replay and does not replace them. The store's
`append_observation` deduplicates stable source identities and `observations`
assigns the project cursor on replay. `provenant events --from SEQ [--follow]`
emits those observations as JSON lines. `--consumer NAME` resumes the stored
cursor and advances it after each line is flushed; a crash before the cursor
write can redeliver that line. The cursor records delivery to stdout, not the
consumer’s processing.

The existing host tick owns ingestion. `domains.wait.forge_source` selects
`auto`, `webhook` or `poll`; source diagnostics and the cached selection retain
its configuration origin. `auto` and `webhook` try the official `gh-webhook`
extension with repository admin permission, warning and falling back to one
shared conditional poller when either is unavailable. The tick starts one
locked host forwarder per project/repository; chairs never own a listener.
Conditional ETag responses preserve the cached resource on HTTP 304. Failed
passes retain progress and back off; a successful pass restores cadence.
An unavailable forge during initial discovery warns and leaves local journal
sources running. Normalisation preserves nested PR associations and available
check names; bounded metadata carries explicit truncation flags.

`event_delivery::Journal` captures a cursor before reconciliation, then wakes
waiters from replayed records. Named PR interests survive waiter expiry; the
host continues indexing their outcomes. `pr watch` pins its head, reconciles
checks initially and after a quiet-period gap, and revalidates the PR head at
the end. A watch timeout reports its last check buckets as a timeout, not a CI
failure. `status_wait`, provider lens waits and owner-side lane waits subscribe
before their initial read. Graph markers index committed checkpoint digests
(`GraphCheckpoint`), and retained run-event ingestion indexes queued, running and terminal
`LaneLifecycle` transitions. Its source cursor and observations commit together;
terminal reactions also retain their independently retryable settlement notice.
Identifier-only local markers trigger a targeted authoritative read; unrelated
events do not trigger one. A gap triggers reconciliation at least every minute,
and timeout retains the last known state. Successful host reads refresh a local
receipt even when HTTP 304 produces no new observation, so quiet, healthy
sources do not cause each waiter to poll GitHub. Peer-owned state still uses its
owner transport, with its declared paced fallback when local events are not
available; a shared journal never widens access to another host.

Forge repository feeds and snapshots are bounded, partial observations, not a
claim of complete GitHub history. The fallback discovers the first page of
open PRs, recent repository events and workflow runs, then retains named PR
subscriptions to track their CI and closure. Settled closed PRs stop active
polling without deleting the subscription or outcome; discovery re-arms a
reopened PR. Comments retain identifiers and
state only, never their text. A consumer reconciles source state after a gap;
it cannot reconstruct comments that fell outside GitHub's retained feed.
The notes event kind is defined here; its source indexing is owned by the notes
slice.

## Shared notes

`fabric_runtime::notes::Board` owns immutable note files beneath
`state/notes/PROJECT`. Host processes publish a complete numbered file with
no-replace atomic publication under a stable board lock; neither corrections
nor read caps rewrite source entries. A correction appends a new entry with
`resolves` naming an earlier source sequence. Tags are unrestricted labels.

`notes::Input` contains content, scope and read controls only. The bound host
relay stamps the attempt, resolved adapter/model, host-observed commit and
wall time; `verified` starts false. Unknown facts remain null. Provenance fields
in a lane request are rejected, including `verified`. Note text remains a claim.
No entry auto-promotes into documentation or issues.

Scopes are logical work paths: project, graph, or graph/node subtree. A project
read sees every project note. A graph read also sees project broadcasts; a
subtree read sees project and graph broadcasts and its ancestors/descendants,
excluding sibling subtrees and other graphs. Descendants follow the graph's
`--` node separator, not arbitrary string prefixes. Projects have separate files.

`notes::Note` retains a stable identity and a source-local sequence. Its
`observation` conversion takes a delivery-allocated `ObservationCursor`, preserving
the shared contract's project-stream ordering and `ObservationKind::Note`.
The delivery slice indexes these source records; it does not replace them.

Reads are delta by default, keyed by host-bound lane, reader alias and scope.
Chair aliases have a separate namespace; a lane cannot advance another lane's
or the chair's cursor by choosing the same alias. A read does not move its
cursor: the CLI ACKs only after successful complete output. Lost replies may
redeliver, with the same note identity. Missing cursors start at zero; malformed
or out-of-range saved cursors replay from zero and disclose recovery. Explicit
`--replay` leaves the delta cursor unchanged. ACKs never move a healthy cursor
backwards. Cursors are disposable; source entries are authoritative.

The default read cap is 50 entries (source `default`); `notes read --limit N`
overrides it (source `launch`). A separate default byte cap bounds a page; a
single entry above that read byte cap is returned whole so the cursor can progress.
Before publication, the board checks that a single-note page fits the existing
result transport ceiling, including pretty CLI output and read metadata. A note
that cannot be delivered is rejected before append. Multi-note pages also obey
that transport ceiling and leave remaining entries for the next page. Every page
reports both caps, source, continuation, truncation and cursor recovery. Nothing
is dropped when a page is capped: continue the delta, or replay explicitly.

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
