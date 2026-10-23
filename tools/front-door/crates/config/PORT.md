# Config port batch 2

Rulebook: `docs/specs/rust-port/PORTING.md` version 4. Source and oracle
revision: `7475ff01e437ec51c760b852516c21a5281cf9c6`. Author route:
`codex/gpt-6-luna@high`. No command is switched over and no stable install is
changed or executed by this lane.

| Unit | Status | Scope and remaining work |
| --- | --- | --- |
| `scripts/configure-codex-skill-overrides.py#unit-01` | Partial | State, proposal, preflight, atomic configuration, check/configure output and exit policy are represented. Argparse/help boundary remains with command integration. |
| `scripts/install-provenant-command.py#unit-01` | Partial | Snapshot classification and absolute-path policy remain from batch 1. Atomic publish/exchange/rollback and its race recovery need the supervisor safe publication API; CLI argument rendering remains. |
| `scripts/install-workflows#unit-01` | Partial | Source validation, receipts, preflight, link publication, repair, retirement, relocation, summary output and exit policy are represented. CLI argument/help boundary and supervisor publication API remain. |
| `scripts/instance_installation.py#interface` | Partial | Typed desired-state contract, mode, explicit environment precedence, custom-skill policy and action/pointer/seed/routing result types. Validation over serde_json's sorted object map can change path-free error precedence from Python insertion order; `~username` expansion is not implemented. |
| `scripts/lib/front_door_build.py#unit-01` | Partial | Workspace digest covers the R2 inputs and matches Python on a fixture with nested packages, `src/target/`, a literal backslash filename, editor litter and excluded build output. Process execution for build/smoke and full selection orchestration need the supervisor runner. |
| `scripts/managed_installation_manifest.py#unit-01` | Partial | Schema-v1 validation, opaque history, custom disjointness, atomic persistence and tree digest remain from batch 1. Wiring the shared records codec requires root workspace dependency declarations outside this lane's write scope; J2 schema coverage is also outstanding. |
| `scripts/manage_installation.py#unit-01` | Partial | Source catalogue and managed/custom collision policy are ported with insertion-order preservation and a Python differential. Link replacement, retired-agent cleanup, and staged directory migration remain. |
| `scripts/manage_installation.py#unit-02` | Skipped | Planning, integrity, conflict rendering and execute actions are not ported; they depend on unit-01 ownership and publication behavior. |
| `scripts/manage_installation.py#unit-03` | Skipped | Remaining execute branches and CLI output are not ported; they depend on unit-02 planning/action behavior. |

## Pending copies

`src/pending.rs` remains private and marks its eventual owner on each item. Its
core path resolver and records codec duplicate cannot be wired to the shared
crates without adding root `[workspace.dependencies]` entries, which are
outside this lane's authorized write scope. Supervisor staging, link
replacement and publication primitives also remain pending their owner.
`src/python_unicode.rs` contains the Unicode 16 non-printable table needed for
Python `repr`; it is marked for `provenant-core` because core's helper is not
exposed to this workspace member.

## Evidence and limits

The new differential oracle uses `PROVENANT_PORT_PYTHON` (default `python3`),
Python `-I`, an explicit environment and a bounded child wait. Missing or old
Python prints `skip:` and fails when `PROVENANT_PORT_DIFF_REQUIRED=1`.

Targeted differentials cover Codex command output/status and proposals,
workflow command output/status, successful receipt publication, conflicting
target refusal, front-door digest input classes and invalid-mode rendering.
The crate suite passes **51 tests** with Python differential required. Open
diagnostic, non-UTF-8 output and interface gaps are recorded in RP-config-4
through RP-config-6. J1/J2 release gates and macOS/Linux publication
conformance are not claimed. No Python source, root binary, graph or CI files
are included.
