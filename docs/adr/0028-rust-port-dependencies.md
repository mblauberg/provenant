# ADR 0028 - Port the whole project to Rust with a small crate allowlist

**Status:** Accepted 2026-10-23 (user direction in session)

**Amends:** [ADR 0027](0027-rust-front-door.md), decision 1 ("no crate
dependencies"). Plan and rulebook: [Whole-project Rust port](../specs/rust-port/README.md),
[PORTING.md](../specs/rust-port/PORTING.md).

## Context

ADR 0027 kept the front door std-only because it only resolved a root and
execed an owner. The port moves every Python and TypeScript owner into the
same Rust workspace, including Fabric's runtime, SQLite store and MCP server.
Doing that with the standard library alone would mean hand-writing a JSON
codec, an SQLite binding, a TOML editor, regular expressions, time zones and
an argument parser. Each is a known source of bugs that mature crates have
already paid for.

The user asked for staple, modern crates only, and for small or replaceable
pieces to be written in-house so the dependency set stays lean.

## Decision

1. **Land continuously.** Port units merge to `main` as small PRs. Each slice ports a
   command or subsystem, switches its callers to Rust and deletes the old
   owner in the same PR; a selector is used only where a slice cannot land
   atomically (amended for rulebook v6 at the user's direction). There is no long-lived port branch.
2. **Fabric's TypeScript runtime is in scope.** The end state has no Node
   runtime; `runtime/fabric` is ported with the rest.
3. **Direct dependencies are limited to this list.** Versions are pinned in
   `[workspace.dependencies]` and `Cargo.lock`.

| Crate | Version | Use |
|---|---|---|
| `serde` (derive) | 1.0 | Typed records, config and protocol messages |
| `serde_json` | 1.0 | JSON everywhere; replaces the hand-written `json.rs` |
| `thiserror` | 2.0 | Typed library errors; compile time only |
| `rusqlite` (`bundled`) | 0.40 | Fabric store; bundling pins one SQLite version on every Mac and in CI |
| `sha2` | 0.11 | Source, record and evidence digests; no hand-written cryptography |
| `regex` | 1.13 | Pattern rules ported from Python's `re` |
| `jiff` | 0.2 | Timestamps, durations and local-time quota schedules from the system tz database |
| `clap` (derive) | 4.6 | Argument parsing for every verb; J1 compares help structurally |
| `toml_edit` | 0.25 | Reading and editing Codex `config.toml` without losing user formatting |
| `libc` | 0.2 | Supervisor syscall layer only (already allowed by the supervisor spec) |
| `tempfile` (dev only) | 3 | Test fixtures |

4. **Written in-house instead of adding a crate.**
   - **HTTP.** Only two HTTPS GETs exist (Claude and Cursor usage). They run
     `curl` with headers passed on stdin, so tokens never reach argv. This
     uses the system trust store and proxy settings and avoids a TLS stack.
     Revisit if HTTP use grows.
   - **MCP.** Fabric serves one stdio transport. A small JSON-RPC 2.0 server
     over newline-delimited stdin and stdout replaces the TypeScript SDK; J3's
     transcripts pin its behaviour. This removes any need for `rmcp`.
   - **No async runtime.** Without `rmcp` and an HTTP client there is no
     need for Tokio. Concurrency uses threads.
   - **Errors at the edge** use `Box<dyn Error>`, not `anyhow`.
   - **Diagnostics** use a small stderr module, not `tracing`.
   - **Random identifiers** take 16 bytes from `getentropy` through `libc`,
     not `uuid` or `getrandom`.
5. **YAML.** Provenant's own YAML data files move to JSON in a separate
   migration lane. This includes `skills/*/evals/*.yaml`, `library/*/evals`,
   `evals/`, `docs/evals/`, `config/adapter-compatibility.yaml` (read at runtime
   by `scripts/model_route.py`), `runtime/*/provenance_components.yaml` and
   `tests/fixtures/*.yaml`. Migrate their readers and callers together.

   Formats owned by other tools remain YAML: GitHub issue forms under
   `.github/` and the setup-repo skill templates, `agents/openai.yaml`
   sidecars and `SKILL.md` frontmatter. `provenant-records` owns an in-house,
   fail-closed subset reader. It accepts block maps and sequences, plain,
   single-quoted and double-quoted scalars, `>`, `>-` and `|` block scalars,
   flow sequences of quoted strings and comments. Plain scalars resolve only
   to `true`, `false` or string. Refuse `yes`, `on`, numbers and null-like
   words with a `quote this` error. Refuse anchors, aliases, tags, flow maps,
   multiple documents, multi-line plain scalars, tabs and duplicate keys.
   Every refusal names the construct and line.

   The reader's CI test must parse every tracked file in those external
   groups and, while Python exists, compare accepted values against PyYAML
   on the same files. Exact PyYAML error text is not a contract; PORTING.md
   records the divergence class. YAML 1.2, PyYAML's YAML 1.1 and GitHub's
   parser do not share exact semantics. A controlled subset needs only a
   few hundred lines, so `saphyr` adds a dependency without solving parity.
6. **Adding to the list.** A new direct crate, a feature expansion or a
   material transitive addition needs a PORTING.md rule change with a reason.
   A crate must be widely used, actively maintained, MIT or Apache-2.0
   licensed, and not cheaply replaceable in-house. No per-lane additions.
7. **Python AST tooling stays outside the port.** No Python parser crate is
   added: the crates.io `ruff_python_parser` is an internal, unstable
   component and `rustpython-parser` is dormant. Neither meets decision 6.
   Graph units marked `retired` carry a deletion criterion and are not Rust
   port assignments. Delete Python-AST tooling when its target glob is empty.

   Retire the unused AST move-comparison helper immediately; its sole caller
   is its own test. Keep `scripts/static-security-check.py` running in Python
   CI until no tracked Python sources remain. Its Rust controls are
   `#![forbid(unsafe_code)]` in every crate except supervisor and a workspace
   `clippy.toml` `disallowed-methods` entry for `std::process::Command::new`
   outside supervisor, where process custody belongs. The command lint is
   separate graph work because existing callers need migration or scoped
   allowances.

   Retire `scripts/lint-timebomb-fixtures.py` for Python and JS/TS targets
   when no `tests/*.py` and no `*.test.ts` remain. Retarget date lint to Rust
   `#[test]` sources using the textual date engine already in `provenant-cli`.

## Consequences

- The install digest must cover every workspace crate, manifest and the
  lockfile before any library is linked into the binary.
- `rusqlite`'s bundled build compiles SQLite's C source, which adds about
  half a minute to a clean build. The cached Cargo target keeps reinstalls
  fast.
- `help` and root resolution stay cheap: they must not open SQLite or start
  threads.
- Rejected: `reqwest`, `ureq`, `rmcp`, `tokio`, `anyhow`, `tracing`, `uuid`
  and `serde_yaml`, for the reasons above. Also rejected: `saphyr` (parser
  semantics cannot provide parity; the controlled subset is small),
  `ruff_python_parser` and `rustpython-parser` (decision 7).
