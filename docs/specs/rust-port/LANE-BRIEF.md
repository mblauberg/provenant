# Rewrite lane briefs

Chairs fill the placeholders and dispatch the text below a brief's `---` as
the lane prompt. Design lanes run read-only apart from their design doc;
build lanes write in their own worktree.

## Design brief

Placeholders: `AREA`, `DOC`, `CRATES`, `SOURCE`, `EXTRA`.

---

Task: write the design for the AREA area of Provenant's Rust rewrite, in
`DOC`. The design covers the crates CRATES. Provenant is being redesigned and
rewritten in Rust in one pass. The Python and TypeScript sources are source
material for what the product does, not a specification. The user wants a
smaller, better product.

Read first, in full: `docs/specs/rust-port/PORTING.md` (version 7; R0 and R1
first), `docs/specs/rust-port/README.md` and
`docs/adr/0028-rust-port-dependencies.md` (dependency list). Then read the old
source for this area: SOURCE. Read the existing Rust in the listed crates,
which earlier inert ports wrote; it is source material, and its Python
emulation is not.

Establish real use before keeping a feature: find its callers in skills,
hooks, scripts, docs and `tools/front-door/src`, and check
`~/.claude/projects` and `~/.codex/sessions` transcripts for actual
invocations (commands run, not files edited). Transcripts are read-only:
search them, never write, move or delete them. Never open credential files,
`~/.ssh`, `~/.aws`, `~/.kube` or `~/.secrets`.

EXTRA

The design doc states, briefly and concretely:
1. What the area is for, in a paragraph.
2. Features kept, and features dropped with the evidence for each drop.
3. Its command subtree and any MCP tools: verbs, flags, output (human and
   `--json`), exit codes. Mark it as a proposal for the command-tree freeze
   in `design/cli.md`.
4. The data model: records, files and layout under the new state root, with
   versioning and which writes must be atomic.
5. The crate API other areas call, and the API it needs from other areas.
6. External contracts it touches (provider wire forms, hook protocols,
   `SKILL.md`, MCP, Git) and the recorded fixtures that test them.
7. The test plan: unit, integration and process tests, per PORTING.md R9.
8. The deletion list: every Python and TS file, test and helper the build
   removes, with line counts, and the expected net line change.
9. Build order: PR-sized steps, each landing Rust and deleting what it
   replaces.

Keep it to what a build lane needs; no history of the old design. Open a PR
to main titled `docs(rust-rewrite): AREA design` containing only `DOC`. Do not
arm auto-merge.

## Build brief

Placeholders: `CRATE`, `BRANCH`, `DESIGN`, `STEP`, `EXTRA`.

---

Task: build step STEP of the accepted design `DESIGN` in the Rust crate
`provenant-CRATE` (`tools/front-door/crates/CRATE/`), and delete the Python
and TypeScript it replaces in the same PR. This is Provenant's one-pass Rust
rewrite under PORTING.md version 7: no transition adapters, no parity with
the old implementation, no Python or V8 emulation.

You are in your own worktree on branch `BRANCH`, started from `origin/main`.
If main moves while you work, merge `origin/main` in; never rebase.

Read first, in full: `docs/specs/rust-port/PORTING.md`, `DESIGN` and
`docs/adr/0028-rust-port-dependencies.md` (dependency list). Read the crate's
current `src/`: reuse what is good, delete what emulates Python or V8.

EXTRA

How to work:
1. Implement the step as the design specifies: typed errors with thiserror,
   serde records, traits only at real seams, data tables for variants, no
   global mutable state. If the design is wrong or silent on something that
   matters, make the smallest sound choice, note it in the PR, and propose
   the design fix.
2. Test the new design (PORTING.md R9). External contracts use recorded
   fixtures of the other tool's real behaviour.
3. Delete the Python/TS this step replaces, its tests and helpers left with
   no caller. Update in-repo callers that still exist. A caller whose owner
   is not yet rewritten keeps calling the old path; if this step removes
   that path, delete the caller's tests and note the break in the PR.
4. Do not touch the primary checkout, the stable install or the old state
   roots. Tests set `HOME` and the state root to temporary directories
   (PORTING.md R9). Never read credentials.

Write scope: the crate, the binary's wiring for this step's verbs, the files
you delete with their tests, callers you update (including CI workflow steps
and path filters for this step's subjects), and `Cargo.lock`. Not other
crates, except a small API the design assigns to this step. Never mute or
weaken `ci-status`.

Checks in `tools/front-door`: `cargo fmt --all`,
`cargo clippy -p provenant-CRATE --all-targets -- -D warnings` and
`cargo test -p provenant-CRATE`, plus remaining Python or TS tests for any
caller you edited. Heavy suites run in CI.

Before opening the PR, have one native subagent review the diff assuming it
is wrong. Leave no rebase, cherry-pick or merge in progress. Open a PR to main
titled `rewrite(CRATE): <step>`; do not arm auto-merge. The body states the
design step, `Deleted:` with files and line counts, `Net lines:` across
Python, TS and Rust, design deviations, and anything left for a later step.

Self-check before review:
- Integers are `i64`/`u64`, never `f64`; no epsilon comparisons.
- No panics on malformed input (slicing, `unwrap`, indexing).
- Bounded waits only; never an unbounded `wait()` on a child.
- No `unsafe` outside `supervisor::sys`; no `Command::new` outside supervisor.
- Serde wire forms for external contracts match the recorded fixtures.
- Tests cover failure and edge cases, not only the happy path.
- No in-repo caller points at a deleted path: grep for the module name,
  script path and any `python3 -m` form.
- macOS: compare canonicalised temporary paths (`/var` links to
  `/private/var`). Repo fixtures use
  `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/..."))`.
