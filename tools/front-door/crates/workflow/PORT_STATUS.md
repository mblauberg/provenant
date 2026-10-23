# Workflow port status

Rulebook: PORTING.md version 4. Batch 3 source/oracle revision:
`b527d675d15c50ee51701bb15db6439c95277918`. The assigned Python files remain
unchanged through merged main `da6b4b376bffbfccab22b334b2920540ad765ee7`. Batch 2 oracle:
`600dec09bcda3b6a86272e391420e89d38777953`; batch 1 oracle:
`e897b10d58b27a46544969594eed08a5ed64c72c`.

These are inert libraries. No command changes runtime owner, and Python and
TypeScript sources remain in place. Typed hosts separate policy from Git,
provider, clock, store, publication and process effects. A host interface does
not imply its production adapter or cut-over is complete.

## Batch 3 completed units

Only these batch 3 IDs enter `ported.txt` and the PR's `Units:` line.

| Unit | Implemented contract |
|---|---|
| `skills/_shared/review_terminal.py#unit-01` | Every terminal shape, verdict, short-circuit, lineage and certification policy path, with source-backed malformed and unhashable inputs. Adapter identities are a data table. |
| `skills/autopilot/scripts/validate_idle_pause.py#unit-01` | Markdown policy, queue cells and malformed rows, ordered reader errors, distinct UTF-8 decode errors, complete argv/help/error/status surface. |
| `skills/orchestrate/scripts/check_herdr_cli.py#unit-01` | Source CLI, binary/reference selection, version and help capability checks, exact diagnostics and status, through an explicit bounded 10-second command host. |
| `skills/natural-writing/scripts/style_lint.py#unit-02` | CLI argument parsing, help, word count, scanning, findings, missing paths and distinct decode errors. Scanner variants remain explicit inputs; unit 01's arbitrary Python regex limitation remains below. |

## Batch 3 partial units

Every assigned unit has a coherent implementation. None is silently skipped.

| Unit | Implemented part and remaining boundary |
|---|---|
| `skills/_shared/portable_workflow.py#unit-01` | Identity, digest, finite JSON context and artifact effects. Missing argv parser and shared Python JSON support for non-finite numbers, lone surrogates and exact decoder diagnostics. |
| `skills/deliver/scripts/authority_mapping.py#unit-01` | Closed schemas, all mapped dimensions, delegations, paths, timestamps, bounds and containment. Raw budget validation retains input order, then sorts successful output. Shared Python JSON non-finite/surrogate/decoder semantics remain. The direct Value convenience API cannot recover insertion order for first-invalid budget diagnostics; use the raw ordered entry point. |
| `skills/deliver/scripts/delivery_receipt_commands.py#unit-01` | Checkpoint, observation and review policy and ordered new JSONL rows. Missing live locked receipt/custody host; existing receipt mutation still traverses sorted `serde_json::Value`. |
| `skills/deliver/scripts/delivery_receipt_process.py#unit-01` | Capture head/tail policy, command validation and bounded process request/host contract. Process-group signals, pipe drainage and reaping need an allowed confinement process adapter. |
| `skills/deliver/scripts/reference_evaluation.py#unit-01` | Ordered template builder, every reference artifact and timestamp binding, with exact artifact differentials. Custom template non-finite/surrogate JSON still needs the shared JSON owner. |
| `skills/natural-writing/scripts/style_lint.py#unit-01` | Built-in source rules, spans, findings, coordinates, word count and real academic wrapper lookarounds. Arbitrary caller-supplied Python lookarounds/backreferences need a Python-pattern matcher owner; Rust syntax is explicit. |
| `skills/orchestrate/scripts/dispatch_preflight.py#interface` | Ordered open task/route records, direct serde and raw validation, recursive duplicate refusal and exact finite wire strings. Shared JSON cannot retain non-finite numbers or lone UTF-16 surrogates. |
| `skills/orchestrate/scripts/git_evidence_packet.py#unit-01` | Header/status/diff streaming and exact shared Python equality. Shared Python JSON acceptance, invalid UTF-8 and decoder diagnostics remain. |
| `skills/orchestrate/scripts/lane_exit.py#interface` | Open ordered exit input, process integers, receipt translation and observer restoration contracts. Scalar dict keys preserve Python identity and duplicate encoded JSON keys. Full Python JSON domain and confinement no-follow capture adapter remain. |
| `skills/orchestrate/scripts/lane_launch.py#interface` | Ordered supervisor outcomes and provider state, notifications, callbacks, bounded line protocol, code 64 unsupported, code 75 stdout fallback, cancellation and warning handling. Shared Python JSON domain and confinement publication reader remain. |
| `skills/orchestrate/scripts/provider_observer.py#interface` | Dynamic ordered summary, model/session/retry/reset fields and exact finite float wires. Non-finite numbers and lone surrogates need the shared JSON owner. Provider execution policy is outside this interface unit. |
| `skills/orchestrate/scripts/writer_lease.py#unit-01` | Git observations, bounded walk policy, retry snapshots, worktree/environment validation and reservation decisions through traits. Safe walk/open/lock and reservation-file adapters are absent through confinement. |
| `skills/session/scripts/compact_reinject.py#unit-01` | State discovery, overlapping command references, transcript reads, replacement decoding and hook policy. Named-user home expansion and shared Python JSON edges remain. |
| `skills/session/scripts/context_audit.py#unit-01` | Traversal, freshness, run/handoff audit and exact ASCII-escaped reports through typed options. Source argv/help/error parser and shared Python JSON edges remain. |
| `skills/setup-repo/templates/claude-tracker-hook.py#unit-01` | POSIX tokenisation, attached comments, tracker policy and exact spaced/ASCII-escaped stdout. Raw event Python JSON non-finite/surrogate/decoder edges remain. |
| `skills/tracker/scripts/steward.py#unit-01` | Ordered queue codec, replay, pruning, digest, Python summary repr and publication policy. Source argv/help/error parser and shared Python JSON edges remain. |
| `skills/_shared/delivery_run_invariants.py#unit-01` | Risk, override, digest, closure and live artifact invariant decisions through a host. Missing permitted policy/config loader and safe scoped live artifact reader. |
| `skills/autopilot/scripts/bootstrap-autopilot.py#unit-02` | Main orchestration, mission/queue/run records, environment decisions and tested filesystem effects behind `BootstrapHost`. Default UTC/entropy/copy-metadata and safe publication adapters remain. |
| `skills/deliver/scripts/delivery_receipt_commands.py#unit-02` | Show, repair and finding handlers; ordered show output. Missing full source `build_parser` and live locked store; the review argument descriptor is only one parser subset. |
| `skills/deliver/scripts/reference_evaluation.py#unit-02` | Deterministic bundles and complete reference-run materialisation policy, with exact run/artifact differential. Missing concrete confinement artifact host. |
| `skills/orchestrate/scripts/lane_exit.py#unit-01` | Observer restoration, malformed-summary fallback, text byte size and bounded retained capture policy. Missing no-follow reads and complete capture replay through confinement; full Python JSON domain remains partial. |

## Shared owners and pending copies

Workflow uses workspace `provenant-core` and `provenant-records` directly.
Python strip, whitespace, repr/str, equality, Unicode predicates, splitlines,
float encoding/timestamps and
checked JSON use those owners. Matching local helpers and the unused config
object reader are removed. No supervisor dependency is added.

The retained `pending` items have no equivalent allowed owner API on this main:

- `provenant-records`: Python JSON encoders, insertion-ordered maps/values and
  retained records, required-nullable serde hook, checked ordered adapters and
  Python scalar dictionary key identity/encoding.
- `provenant-confinement`: `OwnedFileError` for retained contained-path policy.

`custody.py#unit-01` remains partial from batch 1: only
`contained_regular_path` is implemented. Supervisor has SafeFs/SafeDir/SafeFile,
but confinement does not expose that API. Workflow cannot depend directly on
supervisor under R2. No unsafe substitute is introduced. This same missing
confinement boundary limits receipt effects, lane capture reads, writer locks
and artifact publication above.

## Retained completed units

Review ladder unit 01, run gates unit 01, bootstrap unit 01, review panel unit 01
and authority mapping unit 02 retain their previous completed policy scope.
The three cycle interfaces stay absent from `ported.txt`; their finite-wire
improvements do not establish the entire dynamic JSON contract.

## Verification and review

Required source-oracle tests run Python with `-I`, explicit environments,
`PROVENANT_PORT_PYTHON` selection and bounded foreground child execution.
Missing interpreters print `skip:` and fail under
`PROVENANT_PORT_DIFF_REQUIRED=1`. Tests import unchanged source modules; they
never execute/evaluate source text. Explicit clocks and fixtures replace sleeps.
Temporary bootstrap workspaces are canonicalised outside the repository and
Git worktree discovery before effectful bootstrap cases is refused.

Local gates: `cargo fmt --all`,
`cargo clippy -p provenant-workflow --all-targets -- -D warnings`, and
`PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-workflow`.
Final required run: 138 passed, 0 failed, 0 ignored, 0 measured, 0 filtered
out; zero doctests. Formatting, clippy and both R2 layering checks pass.
The resolver leaves Cargo.lock unchanged from merged main.
Heavy repository checks and Linux
coverage run in PR CI. J1/J2/J3 integration and cut-over gates remain separate.

Native adversarial source review covered every work partition. Repairs include
route-deserialisation bypasses, scalar restoration keys, Python ASCII JSON,
reference plan bytes, ISO timestamps, integer counters, registry diagnostics,
ordered receipt output, overlapping path references, attached shell comments,
post-validation budget sorting and inherited Git environment policy. Exact
wire tests compare JSON strings rather than sorted `serde_json::Value`.
Known partials are excluded from full-unit claims. No auto-merge is armed.
