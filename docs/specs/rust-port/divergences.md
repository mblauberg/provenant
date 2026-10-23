# Differential divergence log

Version: 1. No whole-project old/new differential run has been performed by
this documentation lane. An empty log is not parity evidence.

For each mismatch, append a stable ID and the fields below. Keep fixed entries
as evidence; supersede them by reference rather than erasing them. Fixtures
must redact secrets and bind only named volatile fields. Link retained run
artefacts and CI evidence at the exact candidate and oracle revisions.

| Field | Required content |
|---|---|
| ID / unit | `RP-<number>`, graph node ID, rulebook version |
| Class | `bug` (Rust defect), `library` (dependency semantics), `python-bug` (old implementation defect, including TS), or `improvement` (intentional new behaviour) |
| Reproducer | Command/MCP transcript, fixture and normaliser revision; platform, config digest and isolated roots |
| Compared revisions | Pinned old revision/binary digest, new head/binary digest and judge revision |
| Expected / actual | Exact differing bytes or schema/value/effect paths, including exit and stderr |
| Evidence | Old-pass/mutant-fail judge proof and differential run artefacts |
| Disposition | Open, fixed (commit and rerun), or approved exception (approver, authority, scope and surviving gate) |
| Follow-up | Regression case and any rulebook/generator amendment |

`bug` and `library` stay blocking until fixed or explicitly approved as a
boundary change. `python-bug` and `improvement` need user approval before the
new behaviour becomes the contract. No lane can approve its own exception.

Existing front-door exceptions D1–D3 are already recorded in
[ADR 0027](../../adr/0027-rust-front-door.md#intentional-divergences-from-the-python-front-door).
They form the current baseline; they are not newly discovered port mismatches.

## RP-1001 - Python refusal string escaping

- Unit: `scripts/lane_safety_guard.py#unit-01`, rulebook version 2.
- Class: `bug` (fixed; no contract change).
- Reproducer: `cargo test -j 2 -p provenant-cli decisions_match_python_source_on_adversarial_argv`
  from `tools/front-door`, Python 3.14.8 / Unicode 16.0.0, macOS arm64.
  The fixture passes a single U+200B target, with no filesystem or volatile-field
  normalisation and no provider/config access.
- Compared revisions: Python source and policy oracle
  `5c48d4e5567c4801b072bbf88203e88822079ab1`; faulty Rust policy
  `b2ed1154`; fixed implementation and differential fixture
  `d789ac2cef086e955f1ac1ba8fe23c440dc05dbf`.
- Expected: `kill target '\u200b' is not a literal PID`, with ASCII escape
  bytes. Actual: the target contained literal UTF-8 `e2 80 8b` instead.
- Evidence: the retained differential test failed on this exact comparison
  before repair and passes after repair. `repr_and_strip_match_python_on_control_and_unicode_boundaries`
  adds 377 character/string fixtures, including controls and supplementary
  Unicode. The generated printable-property table pins the oracle's Unicode
  version instead of relying on Rust's differing character properties.
- Disposition: fixed in `d789ac2c`. Independent J1 acceptance remains pending;
  these lane differential fixtures do not replace the independent judge.
- Follow-up: move the private compatibility primitives to `provenant-core`
  during integration; retain the differential cases. No rule amendment or
  intentional exception is requested.

## RP-1002 - Cleanup request error classification

- Unit: `scripts/clean.py#interface`, rulebook version 2.
- Class: `bug` (fixed; no contract change).
- Reproducer: `cargo test -j 2 -p provenant-cli invalid_request_error_classes_match_python_streams_and_exit_codes`
  from `tools/front-door`, Python 3.14.8, macOS arm64. Fixtures use only
  invalid arguments, before any plan, Git query or deletion is attempted.
- Compared revisions: Python oracle
  `5c48d4e5567c4801b072bbf88203e88822079ab1`; faulty renderer
  `d789ac2c`; repair and differential test
  `a79987fea684086aa696b4400e3ed265d713d83c`.
- Expected / actual: approval-pair, prune-options and unexpected-selector
  failures use parser usage plus `clean.py: error: ...`, exit 2. Rust's generic
  renderer instead emitted `provenant clean: ...`. Missing-selector text was
  already correct and remains the special case without usage.
- Evidence: native source-parity review identified the classification error;
  four source differential cases now compare stdout, stderr and exit codes.
  Usage is an explicit parser-owner input; usage/help generation is still
  listed as partial, not claimed as ported by this test.
- Disposition: fixed in `a79987fe`; native repair review is clean. Independent
  J1 acceptance remains pending. No intentional exception is requested.

## RP-130401 - Routing wire shapes, order and numeric precision

- Units: `skills/orchestrate/scripts/exec_routing.py#interface`,
  `scripts/model_route_catalog.py#unit-01`, and partial pools/preferences/live
  units; rulebook version 4 (PR #1297).
- Class: `bug` (fixed by the PR #1304 repair; no live cut-over).
- Compared revisions: Python source at `6aa0097083aa0e9f2f69d2d60fe9f8023dcae594`,
  Rust before repair at `914996a4`; repaired Rust and judges are in PR #1304.
- Reproducer: `cargo test -p provenant-routing -j 2` in `tools/front-door`,
  macOS arm64, using the configured Python >=3.12 with `-I`. No path-valued
  fixtures or live providers are involved.
- Expected / actual: fallback policy null was `"default"`; lists were tagged
  and normalized; parsed candidates dropped null effort and dictionary order;
  execution plan fields did not match `candidates()` inputs; family scans used
  unordered maps; empty models read missing patterns; cache snapshots used
  caller-supplied age rather than `observed_at`.
- Evidence: null-policy, empty-pattern and integer-weight regressions failed
  before repair. Exact wire round trips, source differentials, ordered scans,
  integer boundaries and the 86400-second freshness boundary survive as tests.
  Native repair review also caught integer `-0` becoming float `-0.0` and
  range errors in JSON-string policies losing their type; both now have
  witnessed failing regressions and repairs.
- Disposition: fixed; J1/J2/J3 remain cut-over gates. Compatibility primitives
  are private pending copies, marked with their future core/records owner.
- Follow-up: integrate checked integer JSON ingress and Python compatibility
  helpers into the owning crates. `pending::from_json` rejects integers outside
  i64/u64 with typed `NumberOutOfRange`, as required by v4; callers must use it
  rather than an unchecked `serde_json` decoder at JSON ingress.

## RP-130402 - Lazy alias re-expansion across adapters sharing a family

- Unit: `scripts/model_route_catalog.py#unit-01`, rulebook version 4.
- Class: `bug` (open Rust mismatch; catalog unit remains partial).
- Compared revisions: Python source at `6aa0097083aa0e9f2f69d2d60fe9f8023dcae594`,
  Rust in PR #1304.
- Reproducer: family aliases `{"senior":["sonnet"]}` with adapters `a` and `b`
  both declaring that fixed family. Python's lazy generator snapshots the
  expanding candidate set once per adapter, in set iteration order.
- Expected / actual: Python can include `b-a-sonnet` (or `a-b-sonnet`), whereas
  Rust expands only the original names and returns `sonnet`, `a-sonnet`,
  `b-sonnet`. This can change override reservation validation.
- Evidence: source comparison of `family_alias_candidates`; no parity claim
  is made for this multi-adapter case.
- Disposition: open, recorded at the repair request's direction. Matching
  Python's set-order-dependent expansion needs a dedicated policy decision;
  no exception or live cut-over is authorised here.
- Follow-up: retain this counterexample for the remaining catalogue unit work.

## RP-130403 - Exact pool weight numbers under v4

- Unit: partial `scripts/model_route_pools.py#unit-01`, rulebook version 4.
- Class: `improvement` (explicitly requested in PR #1304's repair instructions).
- Compared revisions: Python source at `6aa0097083aa0e9f2f69d2d60fe9f8023dcae594`,
  Rust repair in PR #1304.
- Reproducer: `integer_weights_retain_exact_integer_wire_form`; input integer
  `9007199254740993` formerly became `9007199254740992.0`.
- Expected / actual: v4 preserves integer inputs and named integer weights as
  integers; float inputs remain floats. Python's `weight_value` explicitly
  coerces weights to floats, so exact integer preservation deliberately follows
  the repair request and R4 rather than that coercion.
- Disposition: implemented within the user's explicit numeric repair scope;
  out-of-range integers are refused rather than rounded. No provider routing
  is switched to Rust by this inert crate.
