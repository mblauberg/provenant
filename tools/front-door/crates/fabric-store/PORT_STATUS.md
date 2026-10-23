# Fabric store port status

Rulebook: **PORTING.md version 4**. Batch: `port/fabric-store-2`.
Source/oracle revision: `600dec09bcda3b6a86272e391420e89d38777953`.
`runtime/fabric/src/store.ts` last changed at
`c80d6a7c90c240a6272fdcea1ec4fdc6ce88c74a`. The oracle imports that tracked
TypeScript module unchanged and the schema is included directly from
`runtime/fabric/schema.sql`. This crate was an empty scaffold at batch start;
there was no batch-1 status or implementation on main.

## Unit status

- `runtime/fabric/src/store.ts#unit-01`: **partial**. Agent/project listing,
  explicit close, issue/path work claims, renewal, verification and release are
  implemented with generation fencing, rollback and checked JSON from records.
  Remaining: safe recursive parent creation in `Store::open`, awaiting the
  supervisor API, and `laneTaskId` (covered with unit-03's dynamic row boundary).
- `runtime/fabric/src/store.ts#unit-02`: **partial**. Landing reads, acquire,
  verify, renewal, push hold, release, bounded polling and named-session
  policy are implemented. Message send/inbox policy is implemented with explicit recipient context and
  atomic expiring claims.
  Native UUID defaults await the supervisor entropy API. IDs and clocks are
  explicit inputs in the inert library.
- `runtime/fabric/src/store.ts#unit-03`: **partial**. Inbox claim completion,
  digest grouping, acknowledgements, observation cursors, teams, cooperative
  tasks and activity are implemented. Legacy dynamic terminal-row coercion
  and the default generated task id remain: the current API accepts typed,
  already normalised terminal inputs and an explicit task id. The runtime
  adapter must preserve `laneTaskId`, `String` and `Number` semantics before
  this unit can be marked fully ported.
- `runtime/fabric/src/store.ts#unit-04`: **partial**. Nullable message-link
  migrations, recipient aliases/teams/broadcast policy and diagnostic status/
  doctor queries are implemented. The private main/WAL snapshot, inode/mtime
  version checks, bounded three-attempt copy, absent/error filesystem paths
  and cleanup await the supervisor safe filesystem API. `inspect_snapshot`
  accepts an already isolated snapshot; it does not substitute a live open.

The crate is inert. No command calls it. Tests open temporary databases only.
Opening a database whose parent is missing awaits safe directory creation;
`Store::open` currently requires an existing parent. Work-claim operations,
agent/project listing and a default connection constructor from unit-01 are
outside this batch.

## Pending shared owners

- `pending::Identity`: minimal project/seat/provider view of the runtime
  caller. Main has the broader `fabric-runtime::identity::Identity`, but
  importing that crate here would reverse R2's dependency direction and form
  a cycle. Keep the private-module view until a lower-layer shared contract
  exists; the runtime integration must convert its owned identity explicitly.
- `pending::IdSource`: UUID entropy contract owned by `provenant-supervisor`;
  no unsafe or ad hoc entropy implementation is supplied.
- `pending` ECMAScript trim, numeric-key and UTF-16 slicing helpers belong in
  `provenant-core`, including `pending::js_iso` for full-range `Date.toISOString`. They are
  JavaScript compatibility, not Python copies. The `provenant-core::py`
  helpers landed on main at `2420674b`; their Python trim, slicing, numeric
  and datetime rules do not replace these ECMAScript contracts.

## Evidence and cut-over gaps

Targeted Rust policy tests and TypeScript differentials compare returned
records/errors and every database row plus `sqlite_master` schema. Fixtures
inject time and UUID entropy at those system boundaries only. Node runs with
an explicit environment, a 30-second child deadline and bounded output.
`PROVENANT_PORT_DIFF_REQUIRED=1` refuses absent or old Node interpreters.
J1/J2/J3, platform conformance and live command cut-over remain with their
owners. CI gates the PR; local results do not establish command acceptance.

## Batch verification

- `cargo fmt --all`: passed.
- `cargo clippy -p provenant-fabric-store --all-targets -- -D warnings`: passed.
- `PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-fabric-store`:
  15 tests passed, including seven TypeScript differentials; zero doc tests.
- Oracle: Node v24.15.0, better-sqlite3 13.0.3, SQLite 3.53.4. Differential
  fixtures compare each typed result's serialised JSON bytes as well as
  semantic results, schema and all rows. Covered failures include logging
  rollback, push failure/release warning, stale/expired claims and write-lock
  contention; the Date fixture covers both TimeClip limits and signed years.
- Independent native source review found diagnostic count-key ordering. The
  typed ordered counts record and serialised-record comparisons fix it.
  The same review also confirmed the specific session lookup must filter
  before decoding unrelated rows; that repair is included.
- No unit is fully ported, so `ported.txt` is unchanged. No units were skipped.
  No intentional behaviour divergence or command cut-over is accepted here.
