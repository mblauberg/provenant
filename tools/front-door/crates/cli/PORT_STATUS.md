# CLI port batch 2

Author route: `codex/gpt-6.1-sol@high`. Rulebook: `PORTING.md` version 4.
Source/policy oracle: `7475ff01e437ec51c760b852516c21a5281cf9c6`; the affected
sources are unchanged through merged main `a05bcb46`, except cleanup's retained
context now follows `f0661f88` (#1323). Main also includes the shared Python
helpers and internal workspace dependencies (#1329).
The crate remains inert. Nothing switches command ownership or retires source.

## Completion

Fully ported in this batch:

- `scripts/lane_safety_guard.py#unit-01`: shell command parsing, substitutions,
  quotes, comments, redirections, heredocs, wrapper stripping and literal-PID
  policy. Literal PID zero checks have no integer range or digit limit.
  RP-CLI2-06 records Python's >4300-digit crash and Rust's normal zero/nonzero
  decision as an explicit divergence.

## Partial units

| Unit | Implemented | Remaining |
| --- | --- | --- |
| `scripts/check-provenant-install.py#interface` | Arguments; provider/status/failure contracts; exact strict help text; optional current identity; rendering | Non-UTF-8 output paths need a Python-compatible text boundary. Config, pointer, binary and digest inspection remain owner integration. |
| `scripts/check_harness.py#unit-01` | Decoded sidecar policy, Python scalar/container repr for skill-token membership, invalid-YAML diagnostic rendering | YAML decoding, non-JSON YAML values, traversal, Markdown/link checks, issue forms and route fixtures. The invalid-YAML differential supplies the external parser's diagnostic; it does not prove a Rust YAML parser. |
| `scripts/clean.py#interface` | Arguments, typed integer/duration errors; retained plan/approval/saved-plan/context records; public plan/apply/prune shapes; public-field filtering | Surrogateescaped paths cannot be carried by these UTF-8 record strings. Python-compatible path codec, complete text/JSON rendering and planner/deletion binding remain. |
| `scripts/git_evidence.py#unit-01` | Batch-1 environment, selection and untracked policy retained | Filter discovery, streaming decoder, snapshots, packet/header encoding and atomic publication. Existing batch-1 subprocess reads also await a bounded supervisor binding. |
| `scripts/lane_safety_guard.py#unit-02` | Recursive command decisions, executable-text filtering, raw-pattern backstop and depth refusal | Hook JSON decoder, exact output encoder and process main, including lone-surrogate JSON inputs. |
| `scripts/lib/stale_checkout.py#interface` | Explicit Git request/environment/deadline contract, observations, advisory and child suppression marker | Non-UTF-8 warning paths; concrete safe process-owner binding. |
| `scripts/lib/stale_checkout.py#unit-01` | Local Git query collection, fetch mtime observation, branch/count parsing, warning and once-per-tree policy | Concrete executor binding, non-UTF-8 paths and remaining numeric/clock edge handling. |
| `scripts/provenant_pr.py#unit-01` | Finished buckets, exact required-context matching, settlement/unknown-set policy and exit mapping | Discovery/normalisation, API calls, polling/head movement, rendering and main. |
| `scripts/retry-npm-install.py#unit-01` | Registry signature classifier, Python word/case semantics, Unicode character tail, retry budget/delay/message decisions | Streaming child ownership, forwarding, launch, waits and main. |

The batch-1 projection unit also receives the requested typed UTF-8 decode
error repair and uses the common bounded oracle helper.

## Skipped units

- `scripts/lint-timebomb-fixtures.py#unit-01`, `scripts/movecheck.py#unit-01`,
  `scripts/static-security-check.py#unit-01` and `#unit-02`: Python AST and
  control-flow analysis need an agreed parser; no allowlisted Rust Python
  parser is available.
- `scripts/check_harness.py#unit-02`: traversal/main depends on the incomplete
  YAML and document-check owner above.
- `scripts/public_release_check.py#unit-01` and `#unit-02`: their Git-evidence
  prerequisite is partial; publication/history effects remain deferred.
- `scripts/reap.py#unit-01`, `scripts/worktree.py#interface`, `#unit-01`,
  `#unit-02` and `#unit-03`: process signalling, writer leases, safe removal
  and worktree mutation need the supervisor/confinement ownership boundary.
  No high-risk mutation API is introduced in this batch.

## Shared owners and pending items

The root manifest now exposes internal workspace dependencies. CLI uses
`provenant_core::py` for repr, strip and string/value rendering. Equivalent
pending helpers, the local printable table and its generator section have been
removed.
The existing repr/strip differential exercises those shared exports.

Remaining private `pending` items belong in `provenant-core`: `decimal_digit`,
`python_digit`, `IntegerError`, `decimal_integer`, `python_splitlines`,
`python_word_character`, and Unicode `alphanumeric`. No equivalent exported
helpers exist yet. `IntegerError` remains re-exported for cleanup's typed
failures; PID zero checks no longer parse an integer. The remaining generator
pins Python 3.14's Unicode 16 alphanumeric database.

Supervisor and checked-JSON integration remain deferred owner work. This lane
adds neither a local checked JSON codec nor a replacement process runner.
`LocalGit` exposes the read request to the future bounded executor.

## Verification and review

Pinned Rust 1.94.1; `cargo fmt --all`, warnings-denied crate clippy and
`PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-cli` pass: 45 tests.
Local Python oracle: the repository's Python 3.14 virtual environment, selected
with `PROVENANT_PORT_PYTHON`. The helper uses `-I`, explicit environments,
version checking, file-backed I/O and bounded waits. Missing/old interpreters
print `skip:` and fail when differential coverage is required. The sidecar
oracle follows the same policy when PyYAML is missing. The existing
Rust CI job installs Python 3.14 and requires differential coverage.

Differentials cover the existing guard allow/deny fixtures plus 600 seeded
shell inputs and additional Unicode/heredoc/command-boundary cases; 95 PID
vectors, including 4300-digit ASCII/Unicode zero/nonzero inputs; an explicit
>4300-digit Python-crash/Rust-decision comparison; wrapper newline/Unicode
edge cases; 175 PR-check combinations; registry signature and
word-boundary cases; 11 actual install scenarios; decoded/invalid sidecar
output; cleanup records; real stale Git inspection, linked worktrees and
missing paths; and the retained projection/diff cases. Cleanup's fixture
initialises its own Git repository because sandbox TMPDIR may be inside a
live checkout. This prevents ancestor discovery and keeps observations local.

Native read-only source review assumed the port was wrong. Its completion
review kept the path/YAML/process gaps partial. RP-CLI2-01 records cleanup's
integer bounds;
RP-CLI2-05 supersedes its old PID claims, and RP-CLI2-06 records the Python
PID digit-limit crash. The retry newline false positive was
caught by the differential and repaired; Unicode word categories use the
Python property table. J1/J2 and cross-family acceptance remain cut-over gates;
this lane does not claim them. Heavy checks remain in PR CI.

The PR #1326 repair pass reproduced the wrapper-boundary and large-PID failures
before fixing them. Missing-PyYAML optional and required modes were both run;
required mode fails as intended. A native read-only reviewer checked the repair
and shared-helper migration. Its Unicode-zero coverage note was addressed with
an additional source differential vector. RP-CLI2-07 records the shared-helper
migration and cleanup context drift found by the exact record differential.
