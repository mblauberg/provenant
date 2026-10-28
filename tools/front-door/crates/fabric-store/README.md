# Fabric store

One worker owns one `Store` connection at `<StateRoot>/fabric/fabric.sqlite3`.
Supply a validated, local-filesystem state root, a core clock, cancellation token
and bounded `WaitBudget` in `Request`. The store starts no threads. It creates
private directories and files through supervisor, refuses observed symlinks and
non-private components, and uses SQLite WAL, FULL sync and foreign keys.
The database and any `-wal`, `-shm` or `-journal` companions must be private
regular files with one link, owned by the effective UID. Zero-link observations
are re-read. SQLite can create a rollback journal while switching to WAL before
migrations, so a journal with safe metadata is allowed for recovery.

`send` freezes a typed message and its explicit recipients. The message ID is
its idempotency key: an identical replay succeeds, changed content conflicts.
`read_mail` with `None` uses that recipient's durable cursor; `Some(seq)` replays
history without changing it. `mark_mail_read` advances monotonically up to that
recipient's highest delivery. Actor IDs are bound to external addresses by
fabric-runtime.
Mail and event reads return store-owned `Page` values with integer sequences;
task reads use a task-ID cursor. Each page has a `truncated` flag for the 1 MiB
byte bound; resume after the last returned sequence or task ID.
Mail/task records and `Page` stay in fabric-store. Human inbox records live in
records because runtime, CLI and MCP share their wire contract.

Human requests retain their opening revision, their optional resolution and
their optional closure, with one immutable answer and one outbox record.
`coordination_operation` commits the revision comparison, answer, history,
outbox, addressed response message and receipt together. Migration 8 adds these
tables without changing existing mail or tasks. Previous binaries refuse schema
8, so rollback requires restoring a pre-upgrade store backup or a forward repair;
do not change `user_version` by hand or delete pending answers.

Requests retain history without automatic pruning, up to 10,000 items per
project, including closed items. Further creation is refused without deleting
anything. Request bodies cap at 8 KiB, answers at 4 KiB and options at 16.
History holds at most three request revisions. Inbox pages default to 50 rows,
cap at 100 and stop at 512 KiB of stored request bytes before envelope rendering.
No chair log is stored by this slice. The runtime owns filter/snapshot-bound
cursors and explicit local-only coverage.

Tasks start Open. Claims atomically activate them, increment the persistent
generation and bind actor, token and expiry. Callers supply a lease duration;
claim/fence decisions and mutation timestamps use `Request.clock` inside the store.
Update, verify and renew require the live fence. An unchanged owner can release,
finish or cancel its own expired Active task. All operations compare actor,
generation and token; takeover invalidates every older fence. Release returns to Open; finish permits only Done or Cancelled.
A new claimant can take over after expiry. Tokens come from the service's safe
entropy boundary and are omitted from task reads, events and Debug output.

Every coordination mutation and its event commit together. Event reads replay
ordered project sequences. Consumer cursors are explicit, durable and monotonic.
SQLite BUSY/LOCKED and a companion observed mid-unlink (zero links) retry within
the request deadline. Rollback precedes bounded, cancellable backoff for SQLite retries.
Other commit failures propagate and are never replayed automatically. The edge
must preserve the original IDs if explicitly retrying an uncertain mutation.

APIs use core `WallTime`/`UtcMicros`. Records use nested `RecordHeader::V1`, the
shared canonical encoder and bounded decoder. Future SQL schema versions are refused.

`acquire_claim` takes an entropy-generated core `ClaimId` for a new scope. Exact
scope re-acquisition retains the original ID and increments its counter, even
when the caller supplies a fresh ID. Canonical paths are sorted and deduplicated;
path and worktree scopes conflict in either prefix direction at path component
boundaries. Issue keys conflict exactly within a project. Paths must exist and
be absolute; claim an existing parent for a future file. This deliberately small
choice avoids inventing a filesystem resolution policy for nonexistent paths.
Claims remain advisory and never replace run's writer lock. Expired claims cannot
renew or verify, but the unchanged holder may explicitly release them. Lists omit
tokens, include retained released/expired observations and use a bounded ID cursor.
Acquire prunes at most 100 released claim payloads and their paths in its project,
only when no push operation references them. Migration 4 retains compact scope
identity/generation counters, including counters for pruned records, so reacquiring
never revives an older fence or reuses its generation. Expired unreleased claims
and every operation-referenced claim remain available. Prefix overlap uses the
project/path index for exact ancestors and binary descendant ranges.

On case-insensitive filesystems, overlap detection relies on canonicalisation
returning the same spelling for case aliases such as `/x/Foo` and `/x/foo`.
If the filesystem preserves different spellings through canonicalisation, both
advisory claims can succeed. No case folding is applied: filesystem case and
Unicode rules vary by volume. Run's writer lock remains the writing authority.

`lock_landing` returns a non-cloneable `LandingGuard` backed by OS crate's safe
exclusive OS lock in `fabric/locks/`. It binds database directory identity,
project and fully-qualified branch ref. Validation checks the lock directory's
identity, not the identity of the locked file; the caller must re-verify that binding.
Hold it across the entire push or reconciliation operation, including Git, and
pass it to every landing mutation.
Lock acquisition is bounded by the request budget and never holds a transaction.
A lock holder can read/mutate through another connection to the same database.

Landing acquire freezes checked expected/candidate `GitOid` values and retains a
per-ref generation counter. `prepare_push` atomically freezes the request, its
canonical digest and the operation ID with the landing record. A different
request under the same operation ID conflicts. One generation has one operation;
preparation replay returns its existing phase without another event. Operations
and fences never expose tokens in observations or diagnostics.

| Current push phase | Action | Next phase |
|---|---|---|
| Prepared or Retryable | Explicit Begin, live fence | Pushing |
| Pushing | Confirmed Git success | Committed |
| Pushing | Uncertain | Unknown |
| Pushing or Unknown | Reconcile remote equals candidate | Committed |
| Pushing or Unknown | Reconcile remote equals expected | Retryable |
| Pushing or Unknown | Reconcile other OID | Conflict |
| Pushing or Unknown | Reconcile without proof | Unknown |

All other transitions return a typed error. Pushing and Unknown pin ownership
regardless of TTL and prevent release or takeover. Renewal requires a live fence;
an unchanged expired holder may release only after any unresolved push is closed.
Retryable
requires explicit Begin and a live fence, never an automatic transport retry.
Conflict records divergent remote proof and permits explicit release; no Git is
performed by this crate.

Identical live acquire retries by the same actor/token return the existing fence
without renewing it or appending an event. Landing retries also match frozen OIDs;
a changed expected or candidate OID conflicts, including during an unresolved push.

Migration 7 drops the removed legacy import's table and strips its always-null
`legacy` members from messages, tasks, coordination receipts and committed peer
replies, and deletes the `imported` event of an import that brought nothing
over. While any data that import wrote remains, opening refuses the upgrade with
`LegacyImportData` (service code `upgrade_refused`), naming the table and its
row count. Earlier migrations commit separately, so the store stays at a version
the previous release still opens. The supported remedy is to keep the previous
release or archive the store, as the
[Fabric hosts runbook](../../../../docs/runbooks/fabric-hosts.md#legacy-import-data)
describes. Records no longer decode `legacy` or `legacy_output_path` at all,
even as null, so a row or peer reply that carries either fails to decode. A
binary from before migration 7 may still write a null one after it, so upgrade
every host together.
All mutations and their events share an immediate transaction, injected clock,
budget and BUSY retry. Tests use temporary roots, barrier handshakes, exact clock
boundaries and real SQLite BUSY/FULL failures; no old store or install is opened.
