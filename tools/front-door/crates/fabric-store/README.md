# Fabric store, F1

One worker owns one `Store` connection at `<StateRoot>/fabric/fabric.sqlite3`.
Supply a validated, local-filesystem state root, a core clock, cancellation token
and bounded `WaitBudget` in `Request`. The store starts no threads. It creates
private directories and files through supervisor, refuses observed symlinks and
non-private components, and uses SQLite WAL, FULL sync and foreign keys.

`send` freezes a typed message and its explicit recipients. The message ID is
its idempotency key: an identical replay succeeds, changed content conflicts.
`read_mail` with `None` uses that recipient's durable cursor; `Some(seq)` replays
history without changing it. `mark_mail_read` advances monotonically up to that
recipient's highest delivery. Actor IDs are bound to external addresses by F3.
Mail and event reads return store-owned `Page` values with integer sequences
and a `truncated` flag for the 1 MiB byte bound; resume after the last sequence.
Fabric-only records, `Page` and any future `Cursor` stay in fabric-store until
a second owner needs them.

Tasks start Open. Claims atomically activate them, increment the persistent
generation and bind actor, token and expiry. Callers supply a lease duration;
claim/fence decisions and mutation timestamps use `Request.clock` inside the store.
Update, release and finish require
the live fence. Release returns to Open; finish permits only Done or Cancelled.
A new claimant can take over after expiry. Tokens come from the service's safe
entropy boundary and are omitted from task reads, events and Debug output.

Every coordination mutation and its event commit together. Event reads replay
ordered project sequences. Consumer cursors are explicit, durable and monotonic.
Only SQLite BUSY/LOCKED retries; rollback precedes bounded, cancellable backoff.
Other commit failures propagate and are never replayed automatically. The edge
must preserve the original IDs if explicitly retrying an uncertain mutation.

`import_snapshot` is explicit and one-shot per source ID. It accepts a supplied,
quiescent, sidecar-free old-schema snapshot, including cleanly closed WAL-mode
and rollback databases. It opens SQLite READ_ONLY with an encoded immutable URI
and reads one deferred transaction, without creating source sidecars or performing
recovery. Existing WAL/SHM/journal sidecars return a typed stop-old-Fabric-and-retry
error. No legacy-root lookup exists. Bounds are 10,000 task/delivery rows and
8 MiB selected text; the whole source file has no size cap. Invalid message/task
rows and invalid individual deliveries are skipped with durable reason counts;
valid sibling deliveries survive. Destination imports require an empty project. A failed transaction leaves
no rows, cursor changes or marker. The source ID also binds the old/new project.
Repeated completed imports return their receipt without reopening the source.

Open/blocked tasks created by cut-over are copied regardless of age. Requests
must be unacknowledged and from the inclusive previous 14 days through cut-over.
Blocked becomes Open with a review note;
old owners are cleared. Callers explicitly bind old labels to actor IDs.
Imported IDs are deterministic, domain-separated hashes of source/project/old ID.
Internal links resolve only within the subset; all others retain legacy identity.
Legacy output paths remain labelled legacy data, never readable artefact handles.
Mail cursors start after the imported deliveries; explicit replay remains possible.

Shared state design owns time: APIs use core `WallTime`/`UtcMicros`, not Fabric's
older millisecond sketch. Records use nested `RecordHeader::V1` and the shared
canonical encoder and bounded decoder. Future SQL schema versions are refused.

F2 adds claims/landing; F3 binds identity; F6b owns run-event pull cursors; F8/F9
wire MCP and CLI. Old TS callers for these owners deliberately remain broken
when their removed store/session/graph paths are called. Their affected tests
are deleted with this owner, including Python graph reservation fixture tests.
No adapter, Python/JS emulation, live import or install is included.
