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
