# Workflow port batch

Oracle and source revision: e897b10d58b27a46544969594eed08a5ed64c72c.
Rules: PORTING.md v2 with the user-directed v3 / PR #1294 override:
J1/J2/J3 gate cut-over, not these inert libraries. No commands switch owner.
The source and test oracle are unchanged at this revision. No stable install
or provider configuration is modified or used by these library checks.

## Custody unit 01: partial

Only `contained_regular_path` is implemented. It checks paths but does not
provide a race-free open or an authority to write. The supervisor scaffold at
the oracle revision exposes no safe descriptor-relative filesystem API.
`open_contained_regular`, directory creation, atomic publication, unlinking,
directory sync and bound reads remain blocked on that API. R7 forbids local
unsafe/syscall implementations, and pathname opens cannot substitute for them.
The private `pending` module holds `OwnedFileError`, which belongs in
`provenant-confinement`. No fake successful effects are exposed.

## Portable workflow unit 01: partial

Identity validation and SHA-256 digest are ported. Contract/context validation
and fallback writes are implemented for finite, Unicode-scalar JSON with
representable integers. The source reader also accepts non-finite numbers,
lone surrogate escapes and arbitrary integers. Those cases remain blocked on
the config/records parser API, not approved divergences (RP-1001). The private
`pending::read_python_object` helper belongs in `provenant-config`.
CLI parsing remains for cut-over.
The private `pending::python_json` encoder belongs in `provenant-records`;
it preserves sorted keys, ASCII escaping, spaces and the final newline at
this artifact boundary. It is not a general canonical floating-point codec.

## Review ladder unit 01

Risk tiers, lens sets, blocker reasons, provider lineage and fallback counts
are ported. `tests/generate_policy_fixtures.py` expands the unchanged Python
policy assertions into retained JSON fixtures; Rust compares the same cases
with both their expected values and the live Python implementation.

## Review terminal unit 01: partial

Closed worker-result shape checks, verdict precedence, endpoint/provider/model
lineage, unavailable statuses and certification rules are implemented for
representable JSON inputs. Arbitrary Python exit integers and lone surrogate
strings remain parser integration gaps (RP-1001), so this unit is partial.
Typed worker
and normalised result records use serde. Policy fixtures translate all 17 pure
assertions in `tests/test_review_terminal.py`; the registry/YAML drift test
remains an integration gap. Differential cases also cover every adapter.

## Run gates unit 01

Nested gate approval, release approval and the approved-release plus settled
observation closed-run invariant are ported. Unknown and malformed fields do
not grant approval. No receipt fields are rewritten.

## Bootstrap unit 01

Only lines 1-262 are assigned: constants, exact help/default text, arguments,
diagnostics, Git query, safe read, replace helper and fenced knob parsing.
`main` (mission resolution, creation, substitution, environment checks and
receipt) belongs to later units and is not implemented here. Exact source
help and fallback text are retained as assets, rather than executable Python.

## Verification and remaining gates

The source policy cases are retained as 71 review-ladder fixtures and 17
review-terminal fixtures, expanded without pytest or internal-seam tests.
Rust tests cover every module, including Python differentials for path
refusals, portable output bytes and failures, review decisions, gate fields,
bootstrap argument/help/default text and knob parsing. All subprocesses run
synchronously. No provider runs, clock sleeps, command cut-over or old-source
deletions are involved.

Only CPython's built-in unhashable-TypeError prose is normalised, to the
exception type and exact list/dict kind, because Python 3.14 changed that
wording. Explicit policy diagnostics and artifact bytes remain exact.
The current interpreter is Python 3.14.3; Rust uses pinned 1.94.1.
The non-UTF-8 filesystem fixture skips if the filesystem/sandbox refuses its
name. This headless sandbox refused it; Unicode, hard links, relative symlinks
and root resolution still ran. Linux/macOS CI must exercise that fixture.

J1 CLI/help/environment, J2 schema/canonical-byte mutants, J3 MCP transcripts,
platform conformance and the cross-crate adapter catalogue drift check remain
cut-over/integration gates. None was claimed as run here. The workspace is
inert and the front door still uses its existing owners.

Final local checks: `cargo fmt --all`,
`cargo clippy -p provenant-workflow --all-targets -j 2 -- -D warnings`, and
`cargo test -p provenant-workflow -j 2` passed. The test run had 16 passing
Rust tests, no failures and no ignored tests, plus zero doctests. Native
adversarial source review and its repair-delta review found no remaining
actionable defects within the documented implemented scope. PR CI remains the
merge gate; this lane does not merge or enable auto-merge.
