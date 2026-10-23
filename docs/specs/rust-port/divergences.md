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

## RP-config-1 — workspace source identity

- Unit: `scripts/lib/front_door_build.py#unit-01`; rulebook version 2.
- Class: `improvement`; required by R2 and ADR 0028's accepted workspace
  digest expansion. The new library is inert; the active builder/checker are
  unchanged and must switch together in the integration pass.
- Compared revisions: Python oracle `5c48d4e5`; Rust branch `port/config-1`;
  judge is `front_door::tests::digest_covers_workspace_libraries_and_build_inputs`.
- Reproducer: edit a member's `src/lib.rs`, `Cargo.toml`, or `build.rs`, or
  root `build.rs` / `.cargo/config.toml`, then recompute the source identity.
- Expected / actual: Python's root-only digest stays unchanged; Rust's digest
  changes. Existing framing and root-only fixtures match Python exactly.
- Evidence: the targeted digest test changes and restores each named input;
  `python_digest_differential_without_workspace_members` compares old/new.
- Disposition: required design change, prepared only. Activation awaits the
  workspace builder/checker integration and its stale-library gate.
- Follow-up: carry the identical input set to the active builder and checker
  before linking a library into the installed front door. Build, smoke and
  select execution remain partial until supervisor's safe runner exists.

## RP-config-2 — record codec integration remains partial

- Units: `scripts/install-workflows#unit-01`,
  `scripts/managed_installation_manifest.py#unit-01`; rulebook version 2.
- Class: `library`; unresolved, blocks claiming either unit fully ported or
  activating the new persistence APIs. This is not an approved behaviour change.
- Compared revisions: Python oracle `5c48d4e5`; Rust branch `port/config-1`;
  fixtures live in `pending::tests` in the config crate.
- Reproducer: load an opaque receipt field containing integer
  `18446744073709551616`, a non-finite number, or a lone surrogate; or construct
  a receipt for a Unix path containing non-UTF-8 bytes.
- Expected / actual: Python preserves arbitrary integers, non-finite JSON
  values and surrogate-escaped filesystem strings; the workspace's default
  serde_json model cannot retain all of them. Rust explicitly refuses
  out-of-range integer input and non-UTF-8 path conversion instead of silently
  rounding or replacing bytes. Non-finite/lone-surrogate JSON fails parsing.
- Evidence: `lossless_integer_boundary_is_explicit` and
  `non_utf8_paths_never_silently_change_the_receipt` pin the integration gap;
  ordinary receipts, opaque history, Unicode, float thresholds, ASCII escaping
  and sorted canonical JSON have Python differential tests.
- Disposition: open; affected units are partial. No dependency feature expansion
  is requested or applied by this lane.
- Follow-up: records owner supplies a lossless wire codec under the allowlist,
  and J2 supplies the currently absent installation schemas. The chair owns
  that boundary decision; no command cut-over is included here.

## RP-config-3 — Codex TOML parser diagnostic wording

- Unit: `scripts/configure-codex-skill-overrides.py#unit-01`; rulebook version 3.
- Class: `library`; Python uses `tomllib`, Rust uses `toml_edit`.
- Compared revisions: Python oracle `5c48d4e5`; Rust branch `port/config-1`
  repair of PR #1290. Regression:
  `codex::tests::invalid_toml_keeps_python_prefix_and_typed_source`.
- Reproducer: call Python `state("[skills")` and Rust `codex::state("[skills")`.
- Expected / actual: both reject malformed TOML and carry the stable
  `Codex config is invalid TOML: ` prefix. Parser explanation, source excerpt
  and location formatting after that prefix may differ. Rust preserves the
  typed `toml_edit::TomlError` as its error source.
- Evidence: the regression asserts the prefix and typed variant; the compared
  parser diagnostics are reproducible with the fixture above. This is library
  evidence, not an old/new CLI judge run.
- Disposition: open wording mismatch, recorded for CLI integration; no approved
  boundary exception or cut-over is claimed. The unit remains partial.
- Follow-up: reconcile or obtain approval for parser wording at the CLI edge;
  implement Python's exit-3 `conflicting:` stderr wrapper there and run the
  cut-over judge. The wrapper is not part of this inert library repair.
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
