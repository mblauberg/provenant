# Port lane brief

Chairs fill `CRATE`, `BRANCH`, `UNITS` and `EXTRA`, then dispatch this text as
the lane prompt. `UNITS` names the slice: its commands or entry points and the
graph units they cover (see `docs/specs/rust-port/SLICES.md`). Every self-check item below was a blocking review finding in batch 1.

---

Task: deliver the slice below in the Rust crate `provenant-CRATE`
(`tools/front-door/crates/CRATE/`), switch its callers to Rust and delete the
Python/TypeScript it replaces. This is the whole-project Rust port, run under
PORTING.md R0: parity only at the external contract, Rust-native design
inside it, and deletion in the same PR. The user wants a smaller, better
product, not a line-by-line rewrite.

You are in your own worktree on branch `BRANCH`, started from `origin/main`.
If main moves while you work, merge `origin/main` in; never rebase.

Read first, in full: `docs/adr/0028-rust-port-dependencies.md` and
`docs/specs/rust-port/PORTING.md` (version 6, the rulebook; R0 first). Skim
`docs/specs/rust-port/README.md` (contract boundary table) and the graph
entries for your units. Read the crate's current `src/` and `PORT.md` or
`PORT_STATUS.md`: earlier inert ports may already cover part of the slice.

Slice:

UNITS

EXTRA

How to work:

1. Map the slice before coding. List its contract surface (CLI verbs and
   machine output, records and files it reads or writes, env, exit codes,
   MCP tools) and every in-repo caller (`grep` skills, hooks, scripts,
   `tools/front-door/src`, docs, tests). List what you will delete: the
   Python/TS modules, their tests, helpers left without callers, and code
   that is dead or test-only (delete it rather than port it). A lint,
   validator or test that a Rust type, enum, newtype or the compiler makes
   redundant is deleted, not ported: encode the rule in the type.
2. Make sure a language-neutral judge covers the contract surface: J1
   golden cases (`tests/golden/`), J2 schemas (`schemas/`), J3 transcripts or
   lane conformance. Add missing cases and record them against the current
   implementation before switching.
3. Implement in Rust as a redesign: choose the data model and structure that
   fit Rust (typed errors with thiserror, serde records, traits at real
   seams, data tables for variants, no global mutable state). Reuse earlier
   inert code where it is good and simplify it where it is not. Do not
   emulate Python or V8 internals (`repr`, strip/whitespace classes, digit
   tables, surrogateescape, float rounding, argparse wording) unless the
   contract shows them; then use the single owner in `provenant-core` or
   `provenant-records`. Do not add `pending` copies: add a small missing
   owner API to its owner crate in this slice, or stop and report it.
4. Switch callers: the front door routes the verb to Rust; skills, hooks,
   scripts and docs call `provenant <verb>` instead of the Python/TS path.
5. Delete the replaced Python/TS, their tests and orphaned helpers, and
   update the inventory/graph generator inputs if they list deleted files.
   Rust tests cover policy; Python differential tests are optional
   scaffolding and go with the Python.
6. Record only contract-visible changes in `docs/specs/rust-port/divergences.md`.

Write scope: the crate, owner crates for small missing APIs, the front-door
routing for the slice's verbs, the slice's callers, the files you delete,
their tests, `tests/golden` cases for the slice, `Cargo.lock`, `ported.txt`
and `divergences.md`. Not CI workflows, not other slices' code.

- Crate edges follow PORTING.md R2 (`tests/test_rust_port_layering.py`).
- Checks in `tools/front-door`: `cargo fmt --all`,
  `cargo clippy -p provenant-CRATE --all-targets -- -D warnings`,
  `cargo test -p provenant-CRATE`; plus the golden cases you touched
  (`python tests/golden/run.py --verb V`) and the Python tests that remain
  for any caller you edited. Heavy suites run in CI.
- Before opening the PR, have one native subagent review the diff assuming
  it is wrong, especially for lost contract behaviour and missed callers.
- Leave no rebase, cherry-pick or merge in progress.
- Open a PR to main titled `port(CRATE): <slice>`. Do not arm auto-merge. In
  the body: `Units:` with the fully replaced graph units, `Deleted:` with the
  removed files and line counts, the contract surface and its judges,
  contract-visible divergences, and anything left for a later slice. Append
  the replaced ids to `docs/specs/rust-port/ported.txt`.

Self-check before review:

- `Units:` lists only units whose source is deleted or whose every
  contract-visible behaviour the Rust path now serves. A `#interface` unit is the typed contract for a source cycle; its
  types must match the source's real input and output shapes, with no
  invented fields or variants.
- At the contract only (records, machine output, digests), Python semantics
  where they are visible: truthiness (empty str, 0, 0.0, [], {}
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
- Judges cover failure and edge cases, not only the happy path.
- No caller is left pointing at a deleted path: grep for the module name,
  script path and any `python3 -m` form.
- macOS: `/var` is a symlink to `/private/var`. Compare canonicalised tempdir
  paths. The full Rust test job runs on Linux; the macOS release-build smoke
  job does not run tests. A sandboxed `TMPDIR` can hide this locally.
- Repo-file fixtures use `include_str!(concat!(env!("CARGO_MANIFEST_DIR"),
  "/..."))`. Deep relative `include_str!` paths can resolve through the
  primary checkout because worktrees nest under `.worktrees/`, then fail in CI.
- Regex classes differ: Rust `\s` excludes U+001C to U+001F, which Python's
  `\s` matches, and Rust `\w` differs from Python's on combining marks. Port
  Python patterns with explicit classes from `provenant_core::py`.
- Fractional-second UTC offsets differ across CPython point releases. Use
  the core `py` fromisoformat helper, which probes the interpreter, rather
  than fixed oracle fixtures.
- CI runs a static security check: no `exec`/`eval` of source text in Python
  fixture generators; import modules instead.
