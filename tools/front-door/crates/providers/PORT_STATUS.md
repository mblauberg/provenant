# Provider crate port status

Rulebook: PORTING.md version 4. Author route: `codex/gpt-6.1-sol@high`.
Source/oracle baseline: `b527d675d15c50ee51701bb15db6439c95277918`.
The Python sources remain unchanged. This crate is inert; no command switches
owner. Tests use CPython 3.12+ with `-I -B`, an explicit minimal environment,
bounded version/oracle collection, and `PROVENANT_PORT_PYTHON`.

## Completed in providers batch 3

- `skills/orchestrate/scripts/adapters/agy.py#unit-01`: argv, permissions,
  optional fields and Python timeout coercion, including Unicode decimal
  strings, booleans, signed/fractional inputs and typed invalid/range errors.
- `skills/orchestrate/scripts/adapters/claude.py#unit-01`: argv, OAuth/key
  selection, permission/boundary policy and Python context truth/string
  rendering. Ordered context objects preserve nested dictionary order.
- `skills/orchestrate/scripts/adapters/kiro.py#unit-01`: argv, v3 engine and
  fresh version-bound negative probe; shared core ISO parsing closes RP-1302.
- `skills/orchestrate/scripts/adapters/__init__.py#unit-01`: all seven source
  registry names in source order; one registration row owns capabilities,
  constructor and optional parser hooks. No duplicate metadata table.
- `skills/orchestrate/scripts/cf_dispatch.py#interface`: typed Options,
  Attempt, validated route projection and dispatch receipt. Exact receipt
  bytes retain field order, ASCII escaping and final newline.
- `skills/orchestrate/scripts/provider_output.py#interface`: parsed output
  record, observed-model tuple, clock-explicit parser and fallible catalogue
  seam. These are contract shapes, not completion claims for parser units.

The previously ported OpenCode unit also has all five requested #1322 review
repairs: first-valid-version semantics, arbitrary-width/Unicode version digits,
case-insensitive log levels, empty model omission, EXIT_AFTER_OUTPUT and the
single registry table. Edge fixtures include lowercase logs, trailing version
text, huge majors, Unicode digits and empty model argv.

## Coherent partials retained

- `skills/orchestrate/scripts/adapters/codex.py#unit-01`: complete pure
  configuration policy/receipt assembly, transport/TOML decisions, writable
  directory filtering, permission profiles and argv assembly. Differential
  coverage includes cached config decisions, plugins, unsafe names, all
  sandbox/resume/capability branches, endpoints and signed contexts.
  Remaining: `_discover_mcp_servers`, `mcp_inventory`, cached plan mutation,
  `provenant_state_dir`, UUID allocation, and their host/effect integration.
  Main has `provenant_supervisor::bounded::run_bounded` and `fs::SafeFs`, but
  R2 forbids a direct providers->supervisor dependency, and confinement has
  no bridge for these operations. This lane neither copies nor bypasses the
  existing safe OS implementation. BuildInputs are execution facts, not an
  invented persisted plan shape. Its endpoint_profile None means missing;
  a present null/empty source value must be projected as Some("").
- `skills/orchestrate/scripts/provider_exec.py#interface`: typed real
  `fabric.exec-plan.v1`, applied grants and internally tagged write boundary.
  Real source planner outputs round-trip, including the ordered path->bool
  `shared_write_paths` object and unknown metadata. Remaining: request/execute
  response contracts, including execution and rejection records; these must
  be reconciled with records' owner/J2 before claiming this interface.
- `skills/orchestrate/scripts/provider_output.py#unit-01`: ordered failure
  rules and telemetry classes; variant/model fixes, quota windows, reset
  calculations, ordered event codec and duplicate evidence. Remaining:
  Python's non-finite JSON extensions and complete datetime endpoints/DST
  edge parity (RP-PROVIDERS-3-PARSER). Shared records currently exposes strict
  JSON only; core's exact PythonTimestamp lacks a full-range ISO renderer.
- `skills/orchestrate/scripts/provider_output.py#unit-02`: stream parsing,
  terminal/retry/continuation policy, provider envelopes, provenance from
  events, questions, diagnostic precedence, and model comparison policy.
  Remaining: `_observed_model` rollout scanning/OpenCode export through a safe
  host seam, plus the codec/time edges above. No provider is invoked by tests.

Partial ids stay absent from ported.txt and the PR Units line. No assigned unit
is silently skipped. Non-finite JSON and unresolved boundary mismatches are
unaccepted library differences; this PR introduces no intended behavior change.

## Shared owners and pending items

Adopted `provenant_core::py` truth, equality, str/repr, strip, ISO timestamps
and CPython float-second timestamp rounding, plus
`provenant_records::checked_json`. Deleted replaced truth/equality/ISO/integer
range-check copies, including OpenCode's redundant strip helper.

Private pending items, each marked with its owner:

- core: IntegerError/int coercion, timeout clamp/integer rendering, Unicode
  decimal digit lookup, isprintable (core's exact table remains private), and
  splitlines with/without endings. Adopt owner APIs when publicly available.
- records: only negative integer zero normalization for ordered typed decoding.
  Integer validation stays with checked_json; its normalize function is
  private and its public Value result loses dictionary order. Delete the
  small normalization bridge when records exports checked typed decoding.

## Evidence

- Pinned Cargo/toolchain: 1.94.1. Local oracle: CPython 3.14.8.
- `cargo fmt --all`: passed.
- `cargo clippy -p provenant-providers --all-targets -- -D warnings`: passed.
- `PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-providers`: 33 passed.
- Native adversarial source review found event-kind truthiness, reset traversal
  order, shared-write mapping shape and receipt/metadata field-order errors;
  repairs have exact regression/oracle coverage. Chair owns cross-family review.
- J1/J2/J3, full suites and CI remain landing/cut-over gates; not claimed here.
