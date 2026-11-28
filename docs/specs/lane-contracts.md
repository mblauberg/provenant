# Lane interoperability contracts

Owner: `tools/front-door/crates/records/src/lane_contract.rs`
Anchor: `tools/front-door/crates/records/src/lane_contract.rs`,
`tools/front-door/crates/fabric-runtime/src/notes.rs`

These contracts are shared by admission, delivery and settlement. The records in
`lane_contract.rs` define them and implement none of those flows.

## Launch request

`ChildLaunchRequest` names the parent attempt, its child's narrowed authority and a
`Destination`: the parent's worktree, a managed worktree on a new or existing branch, or a
parent-owned path. Path claims and own-path commits are optional, sourced and off by default;
claims are advisory and an overlap with a live claim warns `PATH_CLAIM_OVERLAP` without
refusing. Admission resolves paths and links and validates
[custody](#worktree-and-branch-custody) and the inherited
[hard boundaries](../adr/0035-three-hard-boundaries-and-flexible-everything-else.md).
The lexical `NarrowedAuthority::is_within` comparison is one check, never a grant: routes must be
a subset, budgets cannot exceed or drop a parent ceiling, paths must lie under an allowed root,
and other inherited boundary restrictions cannot widen. An unknown authority field makes the
comparison inconclusive, and so false.
- Why: a child must not acquire authority its parent did not hold.

## Worktree and branch custody

Owner: `tools/front-door/crates/records/src/custody.rs`

A worktree and its branch carry `WorktreeLease{holder, since, sharing}`, which replaces
`WriterOwnership` as the ownership record. A lease is live while its holder's attempt is
nonterminal and lapses when the holder is terminal (done, stopped, failed or cancelled). Its
state is derived from the attempt on every read, so no sweeper is needed and a crash cannot
leave a stale lease. A holder whose host is unreachable stays live until
`domains.fabric.lanes.lease_unreachable_seconds` (900), then lapses with
`LEASE_HOLDER_UNREACHABLE`.
- Why: custody must survive a restart without a sweeper, and a stopped lane must not hold its branch forever.

Continuing a lapsed lease needs no flag. `--worktree`, `--branch`, `retry`, `resume`, queue
relaunch and `--adopt` take the lease and warn `WRITER_HANDOVER` with the prior lane, its
terminal state and last head, recording a `Handover` fact in the run store. Uncommitted files
left by the prior lane are kept in place, listed in the warning and the new brief footer, and
never reset, stashed or discarded. A head that moved is re-pinned with `WRITER_HEAD_MOVED`, and
an unreadable predecessor record is a typed operation error, not an ownership refusal.
- Why: a lapsed lease is a handover, not a boundary; the prior lane's work is never destroyed.

A second live writer is admitted by `custody::admit_writer` under `lanes.worktree.sharing`
(H/P/L/O, default `warn`): `allow` proceeds and records the co-writer, `warn` proceeds with
`WORKTREE_SHARED` naming the holder and setting, and `refuse` returns
`WORKTREE_SHARING_REFUSED` with `next: provenant wait HOLDER` or `--destination new`. The
owning lane's own-tree setting (`--worktree-sharing refuse` or `provenant lane share refuse`)
is stored in its lease and wins over the other layers for that tree only. `WriterBusy` and
`FacadeError::WriterBusy` remain only for `refuse`; nested provisioning follows the same
admission rule, and path claims stay advisory.
- Why: sharing is a sourced setting that warns and proceeds, not a new hard boundary.

`custody::guard_destructive` runs before any reset, checkout, stash, clean, force-push or
non-fast-forward ref update, branch deletion or worktree removal that would destroy another
live lane's uncommitted files or ref. It refuses as `other_work` with `DESTROYS_LIVE_WORK`,
naming the holder, its state and the files or ref. Adding commits, fast-forward pushes and edits
to other paths are not destruction. A lapsed lease never triggers the guard, though gc moves its
uncommitted files to `<state>/retained/<attempt>/` before removal. Concurrent commits are
protected by git's index lock and ref compare-and-swap, with
`domains.fabric.lanes.index_lock_retries` (5) retries before a `Transient` failure.
- Why: only actual destruction of another live lane's work is one of the three hard boundaries.

| Key | Scope | Default | Values |
|---|---|---|---|
| `lanes.worktree.sharing` | H/P/L/O | `warn` | `allow|warn|refuse` |
| `domains.fabric.lanes.lease_unreachable_seconds` | H | 900 | seconds |
| `domains.fabric.lanes.index_lock_retries` | H/P | 5 | count |

## Observation events

`ObservationEvent` is versioned, with a stable deduplication identity, a project sequence cursor,
lineage, kind and coverage. Delivery is at least once: a consumer persists its cursor after
processing and deduplicates by identity. Order holds within a project stream, not across projects.
Forge events may have no lane attempt, and unknown or partial coverage stays explicit.
The shared stream indexes observations for replay and never replaces its source stores, which stay
authoritative: the project event store (`crates/fabric-store/src/events.rs`), attempt settlement,
the graph `*.events.jsonl` journal, append-only notes, and forge events from the host listener.
`provenant events --from SEQ [--follow]` emits them as JSON lines. `--consumer NAME` resumes a
stored cursor that advances after each line is flushed, so it records delivery to stdout, not the
consumer's processing, and a crash before the cursor write can redeliver a line.
- Why: waiters and consumers need one replayable stream without a second source of truth.

The host tick owns ingestion; chairs never own a listener. `domains.wait.forge_source` selects
`auto`, `webhook` or `poll`, with its origin retained in diagnostics. `auto` and `webhook` try
the official `gh-webhook` extension (repository admin permission) and warn and fall back to one
shared conditional poller when it is unavailable; the tick starts one locked forwarder per
repository. Conditional ETag responses preserve the cached resource on HTTP 304, failed passes
keep progress and back off, and an unavailable forge during discovery warns and leaves local
journal sources running.

Forge feeds and snapshots are bounded, partial observations, never a claim of complete GitHub
history: the fallback discovers the first page of open PRs, recent repository events and workflow
runs, then keeps named PR subscriptions to track CI and closure. A settled closed PR stops
polling without losing its subscription or outcome, and discovery re-arms a reopened PR. Comments
keep identifiers and state only, never text. A consumer reconciles after a gap; it cannot
reconstruct events that fell outside GitHub's retained feed.

Waiters (`wait --pr`, status, provider lens and owner-side lane waits) subscribe before their
first read, then reconcile from `event_delivery::Journal` replay. Named PR interests survive
waiter expiry and the host keeps indexing their outcomes. A PR wait timeout keeps the last
known state, reports the check summary and next step, and is a timeout outcome, not a CI failure;
its budget comes from `--timeout` or `domains.wait` with its source shown, with no second
hard-coded budget. A PR wait pins its subject head at admission; when the PR moves,
`domains.wait.pr_head_policy` settles `pr_head_moved` or re-pins with `PR_HEAD_FOLLOWED`,
and a forge head that was never known warns `PR_HEAD_UNPINNED` until the first sample
pins it. The effective policy and its source travel in the wait JSON.
Identifier-only local markers trigger one targeted authoritative read, and
unrelated events trigger none. A gap triggers reconciliation at least every minute, and successful
host reads refresh a local receipt even on HTTP 304 so quiet, healthy sources do not make every
waiter poll GitHub. Peer-owned state uses its owner transport, with its declared paced fallback
when local events are unavailable; a shared journal never widens access to another host.
- Why: waiting must be cheap, resumable and honest about what it did not observe.

## Shared notes

`fabric_runtime::notes::Board` owns immutable note files under `state/notes/PROJECT`. Host
processes publish a complete numbered file with no-replace atomic publication under a stable board
lock. Corrections and read caps never rewrite source entries: a correction appends an entry whose
`resolves` names an earlier sequence. Tags are unrestricted labels, and projects have separate files.

`notes::Input` carries content, scope and read controls only. The bound host relay stamps the
attempt, resolved adapter and model, host-observed commit and wall time; `verified` starts false,
unknown facts stay null, and a lane request carrying provenance fields (`verified` included) is
rejected. Note text is a claim, and no entry auto-promotes into documentation or issues.
- Why: a note's provenance must come from the host, never from the lane that wrote it.

Scopes are logical work paths: project, graph, or graph/node subtree. A project read sees every
project note; a graph read also sees project broadcasts; a subtree read sees project and graph
broadcasts and its ancestors and descendants, excluding sibling subtrees and other graphs.
Descendants follow the graph's `--` node separator, not string prefixes. A console tree read
carries the requested subtree to each peer; a peer that does not support selection is reported
with partial coverage and a warning rather than hidden.

`notes::Note` has a stable identity and a source-local sequence; its `observation` conversion takes
a delivery-allocated `ObservationCursor` and kind `ObservationKind::Note`.

Reads are delta by default, keyed by host-bound lane, reader alias and scope. Chair aliases have a
separate namespace, so a lane cannot advance another lane's or the chair's cursor by choosing the
same alias. A read does not move its cursor: the CLI acknowledges only after successful complete
output, lost replies may redeliver the same note identity, and acknowledgements never move a
healthy cursor backwards. A missing cursor starts at zero; a malformed or out-of-range one replays
from zero and says so. `--replay` leaves the cursor unchanged. Cursors are disposable and source
entries authoritative.

`notes read --limit N` overrides the default read cap of 50 entries (source `launch`), and a
separate byte cap bounds a page. An entry above the byte cap is returned whole so the cursor can
progress. A note that cannot fit the result transport ceiling (pretty CLI output and read metadata
included) is rejected before append, and multi-note pages obey the same ceiling. Every page reports
both caps, their source, continuation, truncation and cursor recovery; nothing is dropped when a
page is capped.
- Why: a delta reader must never silently lose a note.

## Result projection

`ResultProjection` is now produced by one host-owned builder (`ResultProjection::new`) and
consumed by `output`, `result` and `status --full`. It presents bounded `HostFacts` first and
`LaneClaims` second. Observed commands carry source and completeness, with unknown exit codes left
unknown, and Git, CI and scope evidence state the same; shared-worktree Git facts describe the
worktree, not child authorship. The lane's handoff and verify are claims, never host attestations,
and are labelled as claims when rendered. The default view leads with settlement, verdict, findings and
verify, then a bounded view of the selected result page with both ends and an omitted-middle marker.
`output --final --to FILE` exports the complete sealed lane result across pages for automation. Host
facts sit behind `--full`, and a non-applicable fact is omitted rather than printed as unavailable.
`--tail N` returns at most N bytes and `--tail-lines N` at most N
lines, clamped to the shared line bound for attempts, peers, composites and events. Human output
strips terminal escapes when stdout is not a TTY.
The host builds the projection and bounds its encoded input to the run result limit, reporting
truncation and missing evidence as partial or unknown.
- Why: a reader must be able to tell what the host observed from what the lane asserted.
