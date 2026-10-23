# Config port batch 1

Rulebook: PORTING.md version 3 (PR #1294); judge gates apply at cut-over.
Author route: `codex/gpt-6.1-sol@high`.
Source and policy-test oracle: `5c48d4e5`; source units are unchanged by the
merge of scaffold PR #1289 (`e897b10d`). No stable install is changed or
executed by this lane. All APIs remain inert.

| Unit | Scope delivered | Remaining / gate |
| --- | --- | --- |
| `scripts/configure-codex-skill-overrides.py#unit-01` | Partial: state, proposals, preflight and atomic configuration, conflict and symlink policy | `main`/CLI rendering, including the exit-3 `conflicting:` wrapper, is deferred to command integration |
| `scripts/install-provenant-command.py#unit-01` | Partial: snapshots, classification, absolute-path policy | `publish`, `_exchange`, `_restore_after_mismatch` and `main`; supervisor safe exchange/no-replace/rollback API and race/conformance tests |
| `scripts/install-workflows#unit-01` | Partial: source validation, ownership receipts, preflight, link publication, repair, relocation and retirement | RP-config-2 record codec; J2 installation schema |
| `scripts/instance_installation.py#interface` | Partial: path-free desired-state model/validator, mode/default/custom-skill policies, action/pointer/seed/routing result types | `main`/CLI rendering deferred; runtime seeding/routing/pointer ownership stays in later cyclic implementation/integration units |
| `scripts/lib/front_door_build.py#unit-01` | Partial: workspace source identity, embedded stamps, prebuilt/mode/cargo/smoke policy, build plans | Supervisor build/smoke runner and select orchestration; activate builder/checker together under RP-config-1 |
| `scripts/managed_installation_manifest.py#unit-01` | Partial: schema-v1 validation, opaque history, custom disjointness, atomic persistence and full-tree digest | RP-config-2 record codec; J2 installation schema |

## Private pending primitives

Every item in `src/pending.rs` names its eventual owner:

- core: non-strict path resolution;
- records: sorted ASCII JSON bytes, Python float spelling, schema-one numeric
  compatibility, SHA hex, strict path text and JSON input codec;
- supervisor: create-new staging with fsync and rename, and staged symlink
  replacement. These safe std implementations do not implement command atomic
  exchange or its recovery semantics.

The `pending` module stays private and there are no internal crate dependencies.
Only allowlisted workspace dependencies are used. This does not amend R2's
config dependency direction or create a config-to-supervisor crate edge.

## Evidence and limits

Unit tests translate policy assertions from `test_install_harness.py`,
`test_install_provenant_command.py`, `test_instance_installation.py`,
`test_front_door_build.py` and `test_install_skills.py`. They cover read-only
conflicts, idempotence, symlinks, unmanaged preservation, durable ownership
before link publication, interruption recovery from a persisted fixture,
relocation, retirement, path-free records, opaque fields and tree digests.
Differentials run Python on isolated fixtures and skip only if python3 is
absent. Explicit clocks replace `_now`; there are no sleep assertions.

J1/J2 release gates and platform conformance are not claimed here. The J2
schemas merged with the scaffold do not include these installation records;
this lane does not invent substitute schemas. Source-specific diagnostic
text from TOML/JSON parsers still needs compatibility rendering at the CLI
edge (RP-config-3 records TOML wording; the Python TOML prefix is preserved).
No Python/TS source, root binary, graph or CI changes are included.
The chair supplies cross-family review; this lane uses the requested native
adversarial source review before opening its PR.

Verification on the candidate: `cargo fmt --all`; `cargo clippy -j 2 -p
provenant-config --all-targets -- -D warnings`; `cargo test -j 2 -p
provenant-config`: **41 passed**, including eight Python differential tests,
zero failed/ignored and zero doc tests. The repair adds TOML-prefix and strict
UTF-8 regressions, strengthens the broken-symlink diagnostic assertion and
canonicalises both macOS temporary-path expectations. This repair uses the
existing writable Cargo cache.

Native adversarial review compared the Rust diff with the Python sources and
policy tests. Repairs cover `.js` candidate refusal, public optional-ref
inspection, collision paths retained as `PathBuf`, and custom-skill names kept
as `OsString` with Python-compatible ordering. The repair review is clean.
Non-UTF-8 ordering is tested with pure values because macOS need not permit
creating such filenames. No repository-wide suite or live install was run.
