# Fabric store port status

Rulebook: **PORTING.md version 4**. Batch: `port/fabric-store-3`.
Author route: `codex/gpt-6.1-sol@high`.
Source/oracle: `runtime/fabric/src/store.ts` last changed at
`c80d6a7c90c240a6272fdcea1ec4fdc6ce88c74a`; unchanged by this lane.
Schema: included directly from `runtime/fabric/schema.sql`.
Main's supervisor safe filesystem owner was adopted after merging
`7021182d535f6c730c29713e77d5bca76893fd06`. No rebase or cut-over.

## Unit status

- `runtime/fabric/src/store.ts#unit-01`: **partial**. Constructor bootstrap,
  safe mode-0700 parent creation, close, announcements, seat/project listing,
  issue/path work claims and generation-fenced renew/verify/release are
  implemented. `lane_task_id` is shared with the legacy terminal boundary.
  Valid non-array persisted path JSON and overlap error ordering are preserved.
  Remaining: `workClaims`/`acquireWork` parity for ordered object-valued path
  JSON, ECMAScript JSON number spelling and JSON.parse syntax diagnostics.
  Main's checked-JSON codec deliberately does not supply an ordered ECMAScript
  value/diagnostic API; retain this coherent policy slice as partial.
- `runtime/fabric/src/store.ts#unit-02`: **ported**. Landing leases, bounded
  waits, push cleanup, named sessions, send and inbox policy are implemented.
  UUID generation takes an injected `IdSource` at the source's system boundary,
  including failed sends and session conflicts.
- `runtime/fabric/src/store.ts#unit-03`: **ported**. Claim completion, digest,
  acknowledgements, observations, teams, cooperative tasks and activity are
  implemented. `TerminalRow` retains absent versus null fields, task fallback,
  ECMAScript String/Number coercion and SQLite failure ordering. Generated
  task IDs take the injected UUID prefix. Checked JSON uses the records owner.
- `runtime/fabric/src/store.ts#unit-04`: **partial**. Migration tolerance,
  routing, diagnostic counts/checks, absent/stat-error paths, private main/WAL
  snapshots, exact dev/inode/size/mtime version checks, three-attempt copying,
  query-only inspection and cleanup are implemented. The supervisor's safe
  read-open API refuses final symlinks; TypeScript's copyFileSync follows
  them. Full main/WAL symlink parity needs a safe read-open variant that follows
  the final link and verifies a regular file. No local unsafe or raw OS copy
  substitutes for that missing API. Filesystem binding-only error wording is
  also a compatibility gap (RP-FS3-003 and RP-FS3-006).

No units were skipped. Only complete unit IDs are in `ported.txt`.
Unit-01 was removed from this lane's temporary completion claim after review.
The crate is inert; no command calls it. Tests open temporary databases only.
Each connection has one owning thread at a time; Store is Send and not Sync.
No transaction spans a wait or a push callback.

## Pending shared owners

- `pending::Identity`: minimal project/seat/provider view of the runtime
  caller. Importing fabric-runtime would reverse R2 and form a cycle; runtime
  integration converts its own identity explicitly.
- `pending::IdSource`: injected UUID contract, owned by supervisor. Its safe
  filesystem API has temporary-name entropy, but no UUID API. Generation timing
  and UUID-prefix policy are ported; the store does not implement OS entropy.
- `pending` ECMAScript whitespace, numeric parsing/stringification, UTF-16
  slicing and full-range Date ISO formatting belong in core. These are
  JavaScript contracts; `provenant_core::py` has different Python semantics and
  does not replace them. No private Python compatibility or checked-JSON codec
  is retained; production decoding uses `provenant_records::checked_json`.
- Unit-04 final-link copying awaits supervisor's explicit link-following
  regular-file read capability. The rest uses `provenant_supervisor::fs`
  (`SafeFs`, `SafeDir`, `SafeFile`); no private syscall/lock implementation.

## Review repairs

1. `a71d4cf9` records unit-01's partial scope; subsequent unit commits add its
   remaining policy and adopt safe parent creation.
2. `81fbe0d6` fixes exact extended contention codes and records invalid-wait
   refusals plus binding diagnostics.
3. `eb9f4d31` decodes refusal records first, preserving `reason`.
4. `b658bd09` records the deliberate ECMAScript Number exception.
5. `c7469881` makes absent loaders/spawn errors skip in optional mode and adds
   the required-mode missing-Node self-test.
6. `d8bf48fa` clears push holds on unwinding; explicit cleanup errors propagate.
   Abort/SIGKILL still rely on the persisted bounded fence.
7. `af0c6fad` adds typed errors, diagnostic status, message/task traits, injected
   ID defaults and Send without Sync. Clocks and safe filesystem handles are
   explicit seams too.
8. `34bedbeb` bounds landing polling independently of the injected clock.
9. `3d11bcea` adds Node edge/failure differentials, including cross-thread locks.

## Evidence and cut-over gaps

Tests compare JSON result bytes, every database row, sqlite_master and pragmas
against the unchanged TypeScript Store. New fixtures cover work fencing,
rollback, default ID failure order, dynamic terminal records, routing and seat
errors, live push holds, deterministic wait changes, cross-thread contention,
private WAL snapshots, absent/invalid/stat-error paths, retry bounds and cleanup.
Only explicit fixture paths and the Node loader's unrelated cache directory are
normalised/excluded; retained data and record numbers compare exactly.

Node uses an explicit environment, isolated TMPDIR, a 30-second deadline and
bounded captured output. Optional missing/old interpreters or missing loaders
print `skip:`; required mode fails. Node is v24.15.0, better-sqlite3 13.0.3.
J1/J2/J3 and platform cut-over conformance remain with their owners. CI gates
this PR. The chair owns the cross-family review and auto-merge decision.

## Native review and verification

Native `code-review` inspected source units 01-04, schema, all crate modules and
policy/differential fixtures at `0d6b6199`, route
`codex/gpt-6.1-sol@high`. It found eager persisted-path shape validation; the
repair retains dynamic JSON and source short-circuit/error order, with a new
Node differential. The review also caught dropped ledger entries during merge
resolution; main's ledger is restored in full and all eight lane sections are
appended. Unit-01 stays partial for the wider ECMAScript JSON codec boundary.

Local required checks: fmt, Clippy with `-D warnings`, the fabric-store suite
(32 tests, including 16 Node differentials; zero doc tests), and both R2 layering
checks. The layering functions were invoked directly because pytest is absent.
The chair's cross-family review and exact-head CI remain pending.

Repair review: **clean** at `f98c3e14`, with the same supplied native route.
The reviewer independently ran the persisted-path required Node differential
(1 passed, 31 filtered) and confirmed main `b527d675` is a byte-for-byte ledger
prefix, no main completion IDs were lost, and only units 02/03 were added.
Final crate run after that source repair:
`test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.81s`
Doc tests: 0 passed, 0 failed. The findings, repair and verification evidence
are summarised above; temporary review files are not durable evidence.


## PR #1332 repair pass

The chair supplied a Claude Sonnet cross-family review confirming units 02/03
and the earlier repairs, with the remaining seam, ledger and integer-deadline
work addressed here. This pass keeps the same two complete unit claims.

| Item | Commit or disposition |
| --- | --- |
| 1. Caller push errors | `752a1899` adds `PushError<E>` and removes the test-only error variant. A caller-owned Git error regression checks hold cleanup. `84f28dca` boxes the combined cleanup cause to pass Clippy without discarding either error. |
| 2. API-shape ledger | `f1348cba` records explicit context/ID/clock/cancellation/now inputs in RP-FS3-008, restores activity defaults 50/200 and deletes the duplicate typed terminal acknowledgement. |
| 3. Numeric run directory | `5080c513` verifies pinned better-sqlite3 13.0.3 binds every Number as REAL. Numeric directory differentials pass; changing int32 Numbers to INTEGER would break this oracle. RP-FS3-009 records the evidence and BigInt distinction. |
| 4. Release states | `114fd626` makes invalid states unrepresentable and rejects invalid wire combinations, retaining valid release/refusal bytes and reasons. |
| 5. Contention causes | `7caaabcf` follows `Error::source`; `8a3acd57` handles transparent wrappers that expose SQLite's underlying cause. Exact plain BUSY/LOCKED classification survives both wrapper forms. |
| 6. Session REAL rows | `738f5730` records the chair-offered v4 integer refusal in RP-FS3-010. Both read APIs reject persisted fractional attempts/PIDs with a typed column error. |
| 7. Wait deadlines | `7d9f8dbf` converts fractional seconds once to checked ceil milliseconds, then uses integer deadline, difference and poll-budget arithmetic. The appended RP-FS3-004 amendment supersedes its original deadline exception and cites the chair decision. |
| 8. Durable evidence | This section replaces temporary-file citations with inline findings, dispositions and verification results. |

Current local checks: `cargo fmt --all` and
`cargo clippy -p provenant-fabric-store --all-targets -- -D warnings` pass.
`PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-fabric-store` passes
36 tests, including 16 Node differentials; zero doc tests. Added regressions
cover caller-owned push errors, default limits, persisted REAL refusals and
integer wait boundaries. The existing release and contention regressions now
also cover invalid wire states and nested causes. The numeric-directory cases
extend the required legacy differential. All databases remain temporary.

`ported.txt` is sorted and unique, contains every ID on origin/main, and adds
only units 02/03. No merge is in progress. Exact-head CI remains the landing
gate; auto-merge is not armed.

Native review of this repair delta is clean through `8a3acd57`, including the
transparent-cause follow-up. It independently checked the pinned binding source
and accepted retaining REAL. Its initial ledger wording concern was withdrawn:
the append-only amendment explicitly supersedes the historical deadline claim.
The reviewer did not run checks; the writer ran the prescribed checks above.
Final required crate result:
`test result: ok. 36 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.97s`
