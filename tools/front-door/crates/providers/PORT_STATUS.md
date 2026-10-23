# Provider crate port status

Rulebook version: 4. Source oracle: current branch Python implementation.
Differential tests use CPython 3.12+ through `PROVENANT_PORT_PYTHON` with
isolated mode and a minimal environment.

## This lane

- Fully ported: `skills/orchestrate/scripts/adapters/opencode.py#unit-01`.
- Partial: `skills/orchestrate/scripts/adapters/__init__.py#unit-01`. The Rust
  registry and capability table cover agy, claude, copilot, cursor, kiro, and
  OpenCode. Codex cannot join the builder registry until its argv/profile
  behavior is complete.
- Partial: `skills/orchestrate/scripts/adapters/codex.py#unit-01`. Rust covers
  the clean/inherited configuration decision plus `_executes`, `_toml`, and
  `_toml_table`. Discovery, path checks, complete configuration assembly,
  permissions and argv construction remain.
- Skipped: `skills/orchestrate/scripts/provider_output.py#interface`,
  `#unit-01`, and `#unit-02`; `skills/orchestrate/scripts/cf_dispatch.py#interface`;
  `skills/orchestrate/scripts/provider_exec.py#interface`. These remain
  unported; no API contract or parser behavior is claimed by this lane.

## Existing partials retained

- agy and claude remain partial under RP-1303 (Python numeric input coercions).
- kiro remains partial under RP-1302 (arbitrary timestamp separator parsing).
- The `pending` Python JSON truth/equality and ISO timestamp helpers remain
  until the core crate exposes workspace dependencies usable by this crate.
  The checked JSON copy remains until records can be depended on through the
  workspace dependency table. The root workspace manifest is outside this
  lane's write scope, so these copies are not deleted here.

## Evidence

- `cargo fmt --all`: passed.
- `cargo clippy -p provenant-providers --all-targets -- -D warnings`: passed.
- `PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-providers`: 22 passed.
- J1/J2/J3 remain cut-over gates and are not claimed by this inert crate port.
