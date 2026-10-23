# Fabric store port status

Rulebook: **PORTING.md version 4**. Batch: `port/fabric-store-2`.
Source/oracle revision: `600dec09bcda3b6a86272e391420e89d38777953`.
`runtime/fabric/src/store.ts` last changed at
`c80d6a7c90c240a6272fdcea1ec4fdc6ce88c74a`. The oracle imports that tracked
TypeScript module unchanged and the schema is included directly from
`runtime/fabric/schema.sql`. This crate was an empty scaffold at batch start;
there was no batch-1 status or implementation on main.

## Unit status

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
- `runtime/fabric/src/store.ts#unit-04`: **pending this batch**.

The crate is inert. No command calls it. Tests open temporary databases only.
Opening a database whose parent is missing awaits safe directory creation;
`Store::open` currently requires an existing parent. Work-claim operations,
agent/project listing and a default connection constructor from unit-01 are
outside this batch.

## Pending shared owners

- `pending::Identity`: minimal project/seat/provider view owned by
  `provenant-fabric-runtime`.
- `pending::IdSource`: UUID entropy contract owned by `provenant-supervisor`;
  no unsafe or ad hoc entropy implementation is supplied.
- `pending` ECMAScript trim, numeric-key and UTF-16 slicing helpers belong in
  `provenant-core`. They are JavaScript compatibility, not Python copies.

## Evidence and cut-over gaps

Targeted Rust policy tests and TypeScript differentials compare returned
records/errors and every database row plus `sqlite_master` schema. Fixtures
inject time and UUID entropy at those system boundaries only. Node runs with
an explicit environment, a 30-second child deadline and bounded output.
`PROVENANT_PORT_DIFF_REQUIRED=1` refuses absent or old Node interpreters.
J1/J2/J3, platform conformance and live command cut-over remain with their
owners. CI gates the PR; local results do not establish command acceptance.
