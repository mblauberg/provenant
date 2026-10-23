# Workflow port status

Rulebook: PORTING.md version 4. Batch 2 source/oracle revision:
`600dec09bcda3b6a86272e391420e89d38777953`. The assigned Python sources are
unchanged through merged main `7021182d535f6c730c29713e77d5bca76893fd06`.
Batch 1 oracle remains `e897b10d58b27a46544969594eed08a5ed64c72c`.
These are inert libraries: no command switches owner, no Python or TypeScript
source is removed, and no stable install or provider configuration is used.

## Batch 2 completed units

Only these IDs are appended to `ported.txt` and listed in the PR's `Units:`.

| Unit | Implemented contract |
|---|---|
| `skills/_shared/review_panel.py#unit-01` | Panel specifications, stable slot resolution, override validation, registry extensions, shortfalls, council/breadth reducers and exact policy errors. |
| `skills/deliver/scripts/authority_mapping.py#unit-02` | Containment across every mapped authority dimension, denials, budgets and expiry, preserving short-circuit precedence. Malformed mapped records return typed errors. |

## Batch 2 partial units

| Unit | Implemented part and remaining work |
|---|---|
| `skills/orchestrate/scripts/lane_exit.py#interface` | Checked process and supervisor evidence, open legacy receipt collections and ordered translation are ported. The `observer.json` input accepted by `settle_observer`, malformed restoration/replay paths and complete error domain lack exact contract evidence. |
| `skills/orchestrate/scripts/lane_launch.py#interface` | Ordered string environment pairs, argv, positive capture limits, ceil/finite-positive durations, batch identity validation and notifications are ported. Full supervisor result protocol, including 64 unsupported and 75 stdout fallback, lacks differential evidence. |
| `skills/orchestrate/scripts/provider_observer.py#interface` | Ordered retained summaries and failures, checked u64 token fields and nullable fields are ported. Stderr/retry/large-integer wires have differentials, but all dynamic semantic values, non-finite JSON and lone surrogates are not proved exact. |
| `skills/_shared/portable_workflow.py#unit-01` | Batch 1 identity, digest, context and finite scalar JSON effects remain. Non-finite numbers, lone surrogates and CLI parsing remain (RP-1003). |
| `skills/_shared/review_terminal.py#unit-01` | Batch 1 validation and decisions remain. Generic Python JSON and registry/YAML integration remain (RP-1003). |
| `skills/autopilot/scripts/validate_idle_pause.py#unit-01` | Text fields, heartbeat fallback, queue escaping and fail-closed pause policy are ported. Source reader error aggregation/rendering and CLI remain. |
| `skills/deliver/scripts/authority_mapping.py#unit-01` | Closed schemas, mapped dimensions, delegations, paths, strict timestamps and bounds are ported. First-invalid budget diagnostics lose source insertion order, and the generic JSON boundary remains partial (RP-WF2-001). |
| `skills/natural-writing/scripts/style_lint.py#unit-01` | Shared vocabulary, every built-in pattern, Markdown/LaTeX spans, findings, coordinates and word counts are ported. Arbitrary compiled Python overlay patterns still need equivalent typed matchers; `PatternRule::regex` deliberately accepts Rust syntax (RP-WF2-004). |
| `skills/orchestrate/scripts/dispatch_preflight.py#interface` | Ordered open task/route shapes and exact outcome variants are ported. Companion raw route decoder rejects duplicates, validates source fields and checks integer ranges; unknown non-finite numbers/lone surrogates remain unsupported (RP-1003). |
| `skills/orchestrate/scripts/git_evidence_packet.py#unit-01` | Streaming header, status and diff policy is ported. Generic Python JSON acceptance and built-in decoder diagnostic rendering remain. |
| `skills/orchestrate/scripts/writer_lease.py#unit-01` | Snapshot comparison, retry guard and branch-name policy are ported. Git observations, bounded filesystem walks and reservation/lock effects remain. |
| `skills/session/scripts/compact_reinject.py#unit-01` | Reverse-line streaming and state rendering, replacement decoding and character truncation are ported. Discovery, tool-reference collection and hook entry point remain. |

## Batch 2 skipped units

| Unit | Reason |
|---|---|
| `skills/deliver/scripts/delivery_receipt_commands.py#unit-01` | Receipt mutation and custody adapters remain unported. |
| `skills/deliver/scripts/delivery_receipt_process.py#unit-01` | Bounded process-group execution needs the supervisor/custody owner. |
| `skills/deliver/scripts/reference_evaluation.py#unit-01` | Template-driven reference builder was not reached in this batch. |
| `skills/orchestrate/scripts/check_herdr_cli.py#unit-01` | CLI capability runner and its bounded subprocess adapter were not reached. |
| `skills/session/scripts/context_audit.py#unit-01` | Traversal, freshness and run/handoff audit rules were not reached. |
| `skills/setup-repo/templates/claude-tracker-hook.py#unit-01` | POSIX shell tokenisation and tracker hook were not reached. |
| `skills/tracker/scripts/steward.py#unit-01` | Queue codec, replay and publication effects were not reached. |
| `skills/_shared/delivery_run_invariants.py#unit-01` | Risk-policy loading, timestamps and live artifact custody checks remain. |
| `skills/autopilot/scripts/bootstrap-autopilot.py#unit-02` | Mission creation and environment checks remain; publication needs the safe filesystem API. |
| `skills/deliver/scripts/delivery_receipt_commands.py#unit-02` | Parser/show/repair/finding depend on unported receipt owners. |
| `skills/deliver/scripts/reference_evaluation.py#unit-02` | Materialisation depends on the reference builder and safe filesystem API. |

## Shared owners and pending copies

After #1329, workflow depends on `provenant-core` and `provenant-records`
with `{ workspace = true }`. Python strip, whitespace, repr, exact JSON equality
and checked JSON decoding use those owners; their local copies are deleted.

Retained marked copies have no equivalent API on this main revision:

- `provenant-core`: decimal/digit and word predicates, splitlines. The word
  predicate table is CPython 3.14.3 / Unicode 16.0.0.
- `provenant-records`: Python JSON encoding, ordered maps/values/typed retained
  records, required-nullable serde hook, and an order-preserving adapter that
  applies the shared checked decoder's integer normalisation.
- `provenant-config`: temporary Python object reader.
- `provenant-confinement`: `OwnedFileError` for existing contained path policy.

Custody unit 01 remains partial from batch 1: only `contained_regular_path`.
Main now has supervisor safe filesystem traits and implementations (#1328),
but workflow has not integrated them through its permitted confinement owner.
Path checks do not grant race-free open authority. No local unsafe substitute
is introduced, and this repair does not claim custody effects complete.

## Retained batch 1 scope

Review ladder unit 01 and run gates unit 01 retain their policy ports.
Bootstrap unit 01 retains lines 1-262: constants, parser/help/default text,
Git query, safe read/replace helper and fenced knob parsing. Its main is the
skipped batch 2 unit above. The retained fixtures contain 71 review-ladder
and 17 review-terminal cases; no test seams are copied.

## Evidence and remaining gates

Required source-oracle tests run CPython with `-I`, an explicit environment,
bounded synchronous child execution and `PROVENANT_PORT_PYTHON` selection.
Missing/old interpreters print `skip:` unless `PROVENANT_PORT_DIFF_REQUIRED=1`,
which fails. Current local interpreters: CPython 3.14.3 and Rust 1.94.1.
Fixtures cover refusals, numeric and Unicode edges, insertion order, required
nulls, registry extensions, timestamp boundaries and reverse-line chunk edges.
Only CPython built-in unhashable exception prose is normalised to type/kind;
policy diagnostics and source artifacts remain exact in the implemented domain.
The APFS non-UTF-8-name fixture can print `skip:` when the filesystem refuses
its name; Linux/macOS integration coverage remains outstanding for that case.

Native adversarial source review and repair-delta review found boundary and
notification defects, now fixed with regressions; its remaining raw route JSON
finding is retained as partial. No reviewer model identity is inferred.
RP-WF2-001 through RP-WF2-007 record remaining gaps and repairs.
Independent repair review additionally caught collection-order loss and the
i64::MIN signal-negation edge; raw-wire regressions now cover both, plus -0.

`cargo fmt --all`, `cargo clippy -p provenant-workflow --all-targets -- -D warnings`
and `PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-workflow` are the
required local checks. The latest required run has 63 passing Rust tests, no
failures or ignored tests, and zero doctests. PR CI remains the merge gate.
J1 CLI/help/environment, J2 schema/canonical-byte mutants, J3 MCP transcripts,
platform conformance and catalogue drift remain cut-over/integration gates.
The launch and supervisor draft schemas differ from actual source records
(RP-WF2-002); those schema gaps are not silently treated as cut-over passes.

## Review repair disposition

The confirmed review-panel and mapped-authority units remain complete. All
three cycle interfaces are deliberately removed from `ported.txt` and the PR's
`Units:` line until their entire source contracts have differential evidence.
No runtime owner changes and no auto-merge is armed. Each numbered repair has
a separate commit; the PR body maps the repairs and review follow-ups.
