# Port lane brief

Chairs fill `CRATE`, `BRANCH`, `UNITS` and `EXTRA`, then dispatch this text as
the lane prompt. `UNITS` comes from `python3 docs/specs/rust-port/ready.py
CRATE 12`. Every self-check item below was a blocking review finding in batch 1.

---

Task: port the units below into the Rust crate `provenant-CRATE`
(`tools/front-door/crates/CRATE/`). This is one batch of the whole-project Rust
port. The user has accepted the plan: land through continuous PRs to main, port
Fabric's TypeScript runtime as well, and use a small crate allowlist.

You are in your own worktree on branch `BRANCH`, started from `origin/main`.
If main moves while you work, merge `origin/main` in; never rebase.

Read first, in full: `docs/adr/0028-rust-port-dependencies.md` and
`docs/specs/rust-port/PORTING.md` (version 4, the rulebook). Skim
`docs/specs/rust-port/README.md` and the graph entries for your units in
`docs/specs/rust-port/graph.json`. Read the crate's current `src/` and
`README.md` or `PORT_STATUS.md`: partial units from batch 1 continue from there.

Units, in dependency order:

UNITS

EXTRA

How to work:

- Write scope: only `tools/front-door/crates/CRATE/**`, that crate's
  `Cargo.toml` (dependencies from `[workspace.dependencies]` only, as
  `{ workspace = true }`), appends to `docs/specs/rust-port/divergences.md`
  and `ported.txt`, and a regenerated `Cargo.lock`. Do not edit other crates,
  the root binary, Python or TypeScript sources, CI or the graph.
- Depend on other crates directly: internal crates are workspace
  dependencies (`provenant-core = { workspace = true }`), limited to the edges
  PORTING.md R2 allows; `tests/test_rust_port_layering.py` enforces them. Use
  the shared owners on main, including `provenant_core::py`,
  `provenant_records::checked_json` and the supervisor's safe filesystem API,
  and delete this crate's `pending` copies they replace. Only when the owner
  has no such API on main yet, define a minimal private version in `pending`,
  mark each item `// port: belongs in provenant-<crate>`, and list it in the
  PR.
- Refactor as you go, not line for line. Keep every observable behaviour
  (outputs, record fields, errors, exit codes and effects) and use idiomatic
  Rust: typed errors (thiserror), serde records, no global mutable state, no
  copied test seams. Record every intended behaviour change in
  `divergences.md` with its class.
- Tests: translate the Python/TypeScript tests that assert policy for these
  units into Rust tests in the crate. Skip tests of internal seams. Add
  differential tests that run the Python or TS implementation on the same
  fixtures. Oracles use `PROVENANT_PORT_PYTHON` / `PROVENANT_PORT_NODE`, run
  Python with `-I` and an explicit environment, print `skip:` when the
  interpreter is absent, and fail when `PROVENANT_PORT_DIFF_REQUIRED=1`. Use
  fixtures and explicit clocks; no sleeps as assertions.
- Nothing calls these crates yet. Do not switch any command over and do not
  delete Python or TS sources.
- One logical commit per unit, with the unit id in the commit body.
- Port as many units as you can in this lane; this is a large batch on
  purpose. Do not stop after the first finished unit: keep going until every
  listed unit is fully ported or blocked by a named missing API on main. If a
  unit is too big, port a coherent part and note what remains. Do not stop
  the batch for one unit.
- Design for extension. Put a trait at each seam where the source already
  varies (adapters, providers, stores, hosts), and describe per-variant
  behaviour in data tables with one source of truth, so a new variant is a
  table row plus a small impl rather than another branch in a central
  match.
- Checks in `tools/front-door`: `cargo fmt --all`,
  `cargo clippy -p provenant-CRATE --all-targets -- -D warnings`,
  `PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-CRATE`. Use the
  pinned toolchain (`~/.cargo/bin/cargo` if cargo is not on PATH).
- Before opening the PR, have one native subagent review the diff against the
  source, assuming the port is wrong. Fix what it finds.
- Leave no rebase, cherry-pick or merge in progress.
- Open a PR to main titled `port(CRATE): <short summary>`. Do not arm
  auto-merge: the chair arms it after the cross-family review. In the body, a line
  `Units: <id>, <id>, ...` lists only fully ported units; then partial and
  skipped units with reasons, `pending` items, divergences and test counts.
  Also append the fully ported ids to `docs/specs/rust-port/ported.txt`.

Self-check before review:

- `Units:` lists only units with every function, branch and error path
  ported. A `#interface` unit is the typed contract for a source cycle; its
  types must match the source's real input and output shapes, with no
  invented fields or variants.
- Python semantics, not Rust defaults: truthiness (empty str, 0, 0.0, [], {}
  and None are falsy), `str()`/`repr()` (True, None, single quotes, repr
  escapes), `str.strip` (also strips U+001C to U+001F), `==` across int, float
  and bool, dict insertion order (no HashMap or BTreeMap where order is
  observable), and a null key present versus absent.
- Integers are i64/u64, never f64; out-of-range is a typed
  `NumberOutOfRange`. Compare exactly, with no epsilon. Float-second
  timestamps use the one CPython-compatible helper owned by `provenant-core`.
- No panics on malformed input (slicing, unwrap, index): return what Python
  returns or a typed error.
- Bounded waits only; never an unbounded `wait()` on a child.
- Wire forms: serde enums serialise exactly like the source JSON, with no
  externally tagged wrappers.
- Differential tests cover failure and edge cases, not only the happy path.
- macOS: `/var` is a symlink to `/private/var`. Compare canonicalised tempdir
  paths. CI runs Rust on Linux only and will not catch this, and a sandboxed
  `TMPDIR` can hide it locally.
- CI runs a static security check: no `exec`/`eval` of source text in Python
  fixture generators; import modules instead.
