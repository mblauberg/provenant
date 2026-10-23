# CLI port batch 2

Author route: `codex/gpt-6.1-sol@high`. Rulebook: `PORTING.md` version 4.
Source/policy oracle: `7475ff01e437ec51c760b852516c21a5281cf9c6`; the affected
sources are unchanged through merged main `cffeb4a54b6a702a008dbfee92bef5177803e8f3`.
The crate remains inert. Nothing switches command ownership or retires source.

## Completion

Fully ported in this batch:

- `scripts/lane_safety_guard.py#unit-01`: shell command parsing, substitutions,
  quotes, comments, redirections, heredocs, wrapper stripping and literal-PID
  policy. Python's 4300-digit conversion limit has a typed error. Integers
  beyond u64 follow the approved R4 `NumberOutOfRange` improvement.

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

The root manifest has no internal `[workspace.dependencies]` entries. Under
this lane's workspace-only dependency and crate-only write scope, the public
supervisor APIs and `provenant_records::checked_json` cannot be linked. This
lane does not add a local checked JSON codec or a replacement process runner.
`LocalGit` exposes the exact read request to the future bounded executor.

The existing private `pending` items still belong in `provenant-core`:
`decimal_digit`, `python_digit`, `python_repr`, `python_strip`, Unicode
`printable`, and their tables. Additions are `IntegerError`, `decimal_integer`,
`python_splitlines`, `python_word_character`, Unicode `alphanumeric`, and
`python_value_repr` for sidecar skill-token membership. The latter is limited
to that membership operation; sorted object members/number spelling are not a
public repr contract. `IntegerError` is re-exported for typed public failures.
The generator pins Python 3.14's Unicode 16 database. Shared public helpers are
not present on this merged main; remove pending copies when those APIs land.

## Verification and review

Pinned Rust 1.94.1; `cargo fmt --all`, warnings-denied crate clippy and
`PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-cli` pass: 44 tests.
Local Python oracle: the repository's Python 3.14 virtual environment, selected
with `PROVENANT_PORT_PYTHON`. The helper uses `-I`, explicit environments,
version checking, file-backed I/O and bounded waits. Missing/old interpreters
print `skip:` and fail when differential coverage is required. The existing
Rust CI job installs Python 3.14 and requires differential coverage.

Differentials cover the existing guard allow/deny fixtures plus 600 seeded
shell inputs and additional Unicode/heredoc cases; 90 PID vectors; wrapper
newline/Unicode edge cases; 175 PR-check combinations; registry signature and
word-boundary cases; 11 actual install scenarios; decoded/invalid sidecar
output; cleanup records; real stale Git inspection, linked worktrees and
missing paths; and the retained projection/diff cases. Cleanup's fixture
initialises its own Git repository because sandbox TMPDIR may be inside a
live checkout. This prevents ancestor discovery and keeps observations local.

Native read-only source review assumed the port was wrong. Its completion
review kept the path/YAML/process gaps partial. The missing CLI integer-bound
divergence is now recorded as RP-CLI2-01. The retry newline false positive was
caught by the differential and repaired; Unicode word categories use the
Python property table. J1/J2 and cross-family acceptance remain cut-over gates;
this lane does not claim them. Heavy checks remain in PR CI.
