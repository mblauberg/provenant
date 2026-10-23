# CLI port batch 1

Rulebook: PORTING.md version 2. Source and policy-test oracle:
`5c48d4e5567c4801b072bbf88203e88822079ab1`. These seven sources are unchanged
in the merged scaffold, `e897b10d58b27a46544969594eed08a5ed64c72c`.
No command imports this crate. There is no cut-over or source retirement.

## Implementation coverage

| Unit | Implemented | Remaining |
| --- | --- | --- |
| `scripts/check-provenant-install.py#interface` | Strict argument, ordered provider/status/error rendering and exit decisions | Partial: config/env/path/pointer/binary inspection, source digest and routing owners are not available as main-crate APIs. No live provider files are read. |
| `scripts/check_harness.py#unit-01` | Decoded sidecar policy, field order, character lengths and relative asset observations | Partial: traversal, Markdown/link checks, issue forms, route fixtures and YAML loading. ADR 0028 requires a separately agreed YAML reader. |
| `scripts/clean.py#interface` | Typed request/operation, approval-pair and prune-selector checks, duration parsing, removal and operational-error text | Partial: plan/dry-run and JSON report contracts, usage/help generation, planner/deletion owner integration. No cleanup is performed. |
| `scripts/git_evidence.py#unit-01` | Closed child environment, Git read errors, revision/path validation, scoped untracked rejection | Partial: filter discovery, streaming UTF-8 decoder, snapshots, header records, packet encoding and publication. Supervisor has no safe atomic-publication API on main. No packet API is exported. |
| `scripts/lane_safety_guard.py#unit-01` | Wrapper/option stripping and literal PID refusal decisions | Partial: shell lexer, substitutions, heredocs and hook execution. This coherent policy slice does not claim to replace `deny_reason`. |
| `scripts/lib/projection.py#unit-01` | Marker validation, full-document check/write, newline handling, source-labelled unified reports | Implementation complete; independent J1/J2 acceptance is still pending. |
| `scripts/lib/stale_checkout.py#interface` | Typed observations, clock-valued warning decisions, once-per-tree renderer and child suppression marker | Partial: bounded/sanitized local Git collection and process-edge environment forwarding. Callers must share one `Advisory` and forward `CHILD_MARKER`; no global environment mutation is used. |

No source unit is retired by this PR. Partial units remain unclaimed as complete.
The missing-judge escalation was sent to the chair through Fabric, message
`1bda6b39-3b7c-4261-8247-3afb5f6a6efb`. J2 schemas arrived with the scaffold
merge; J1 is still PR #1281. There is no independent coverage manifest for
these CLI records on this branch. Independent gates and the chair's stable
install identity are required before acceptance/cut-over. The lane did not
inspect or modify the running installation.

## Pending owner items

The private `pending` module holds only Python text compatibility primitives:
`decimal_digit`, `python_digit`, `python_repr`, `python_strip`, and `python_unicode::printable`
(including their property tables). Every item is marked as belonging in
`provenant-core`; no pending cross-crate dependency is linked. Unicode tables
pin Python 3.14's Unicode 16 database. `pending/generate_unicode.py` regenerates
the printable-property table deterministically; runtime and tests use Rust.

## Evidence

Targeted checks: pinned Rust 1.94.1, two cargo jobs, fmt, warnings-denied clippy,
and crate tests. Tests do not run providers, delete worktrees, fetch remotes,
mutate process environment or sleep as assertions. Python is used only in test
oracles, skipped with an explicit diagnostic if the interpreter is absent.

Differential tests exercise source environment filtering, 90 adversarial PID
argument vectors, 121 diff fixtures (including 199/200-line transitions), 18
projection check/write/error fixtures and a local stale-checkout fixture with
an explicit clock and fetch mtime. Source policy tests are retained unchanged.
A non-printable Unicode refusal mismatch was caught and fixed; see RP-1001 in
the divergence log. No intentional behaviour change is proposed.
