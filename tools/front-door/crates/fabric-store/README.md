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
tables without changing existing mail or tasks. Migration 9 adds the immutable
`run_chairs` association table. The runtime opens it only through `HostStore::open`
at `<StateRoot>/fabric/host.sqlite3`, with the same safe opening, companion checks
and migrations, without opening the lane-writable `fabric/locks` directory.
Only the typed host handle exposes chair reads and writes; ordinary `Store`
handles cannot access those APIs. Warm opens skip migration write transactions.
Lanes can write the coordination database and child run stores;
their exact grants exclude `host.sqlite3` and its companions. The authoritative
index seals project/run, request digest and optional chair. Readers ignore any
association rows in lane-writable databases.
Migration 10 adds the partial destination index for local answer observations.
Migration 11 adds a queued-only index. Migration 12 adds hook indexes that
exclude terminal hook failures. Previous binaries refuse schema 12, so rollback requires restoring a pre-upgrade store backup or a forward repair;
do not change `user_version` by hand or delete pending answers.

Requests retain history without automatic pruning, up to 10,000 items per
project, including closed items. Further creation is refused without deleting
anything. Request bodies cap at 8 KiB, answers at 4 KiB and options at 16.
History holds at most three request revisions. Inbox pages default to 50 rows,
cap at 100 and stop at 512 KiB of stored request bytes before envelope rendering.
No chair log is stored by this slice. The runtime owns filter/snapshot-bound
cursors and explicit local-only coverage.

`offer_human_answers` records offers in an immediate transaction, serialised
with acknowledgement. Runtime validates the exact registered target first.
Offers contain at most 100 answers and 8 KiB of compact JSON; overflow keeps its
previous state. Acknowledged answers never return. Pending answers remain
eligible after restart or adapter failure. Mail read cursors stay independent.
This uses the existing outbox without another schema migration.
`offer_human_answers_once` instead uses bounded selected-key reads and one immediate
transaction, with no busy retries, creation or migrations. Only queued records
transition; matching offered snapshots replay the committed record idempotently.
The hook gives queued rows priority and then retries committed offers using
migration 12's indexes.
A late commit or failed stdout write remains re-offerable to the exact session
until acknowledgement. Consumers deduplicate by answer ID before taking action.
Invalid hook candidates are skipped with an answer-ID diagnostic; ordinary
observation readers return an error for invalid records.
`read_hook_answer_page` returns a continuation after the last scanned key,
including skipped invalid or oversized rows. Hooks use it to advance past
pages with no deliverable answer while retaining the same target and deadline.
Answers proved too large for every hook context are marked in the delivery
error field when it is empty and excluded from subsequent hook scans, while
their state and Fabric mail remain. Context-dependent failures remain eligible.
Hook replay preserves `attempts` and `last_attempt_at`: those fields record the
queued-to-offered transition, rather than counting injections or replays.

`Store::open_read_only` never creates, migrates or enables WAL. It opens the
database, WAL and shared memory read-only (`readonly_shm=1`), starts one read
snapshot and sets a SQLite progress deadline without busy retries. Missing or
uninitialised WAL shared memory fails closed. `read_human_answers` requires
the migration-10 index and reads at most limit+1 pending rows for an exact
host/project/session/actor. It leaves state, attempts and timestamps untouched.
The composition edge validates the current session-discovery registration.
The private SQLite VFS forces main/WAL file opens to read-only and refuses
creation, temporary files, journals and deletion. Normal coordination writers
retain WAL/shared-memory companions and truncate WAL after the final checkpoint,
so the idle reader works without allocating them.

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

Migration 7 removes the retired legacy import table and fields. Stores carrying
legacy markers can migrate; the migration removes those retired fields.
All mutations and their events share an immediate transaction, injected clock,
budget and BUSY retry. Tests use temporary roots, barrier handshakes, exact clock
boundaries and real SQLite BUSY/FULL failures; no old store or install is opened.
