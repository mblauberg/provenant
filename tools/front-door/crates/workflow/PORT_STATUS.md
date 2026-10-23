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

## Portable workflow unit 01

Identity validation, object reads, SHA-256 digest, contract/context validation
and fallback artifact writes are ported. CLI parsing remains for cut-over.
The private `pending::python_json` encoder belongs in `provenant-records`;
it preserves sorted keys, ASCII escaping, spaces and the final newline at
this artifact boundary. It is not a general canonical floating-point codec.

## Review ladder unit 01

Risk tiers, lens sets, blocker reasons, provider lineage and fallback counts
are ported. `tests/generate_policy_fixtures.py` expands the unchanged Python
policy assertions into retained JSON fixtures; Rust compares the same cases
with both their expected values and the live Python implementation.

## Review terminal unit 01

Closed worker-result shape checks, verdict precedence, endpoint/provider/model
lineage, unavailable statuses and certification rules are ported. Typed worker
and normalised result records use serde. Policy fixtures translate all 17 pure
assertions in `tests/test_review_terminal.py`; the registry/YAML drift test
remains an integration gap. Differential cases also cover every adapter.

## Run gates unit 01

Nested gate approval, release approval and the approved-release plus settled
observation closed-run invariant are ported. Unknown and malformed fields do
not grant approval. No receipt fields are rewritten.
