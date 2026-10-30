# Maintaining Provenant

Read [`AGENTS.md`](AGENTS.md), [`HARNESS.md`](HARNESS.md) and
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) before changing the harness.
Inspect the live diff and preserve unrelated work. `HARNESS.md` owns lifecycle,
risk, authority, orchestration, review policy and user gates;
[`docs/worktrees.md`](docs/worktrees.md) owns branch and linked-worktree
operation. Use the repository process declaration below for issue intake,
workflow state and pull-request linking.

Agent merges are authorised for this repository; mechanics remain in the
GitHub work-item workflow. This is Provenant-local process, not harness
doctrine.

## Repository process

### Tracker

- Choice: `github-issues`
- Pointer: <https://github.com/mblauberg/provenant/issues>

### Scope and stories

- Canonical home: issue-tracker
- Pointer: <https://github.com/mblauberg/provenant/issues>

Change scope and stories live in the parent issue when the repository declares
`issue-tracker`; project documents retain durable intent, decisions and
invariants. A repository declaring `project-docs` uses its documented scope or
story home instead. A link-only map is a fallback for unavailable or
cross-tracker routes, not a second current-work owner.

### Workflow state

- Owner: tracker
- Pointer: <https://github.com/mblauberg/provenant/projects/2>

### Docs layout

- Pointer: [`docs/`](docs/)

### Change history

- Owns: `git log` for what changed and when; [`docs/adr/`](docs/adr/) for why
- Living docs are edited in place, not appended to; see
  [engineering-docs](skills/engineering-docs/SKILL.md#binding-rules)
- Release notes: each user-visible change adds a fragment under
  [`docs/release-notes/next.d/`](docs/release-notes/next.d/). Fragment authors
  and the release collator follow the
  [release-notes reference](skills/engineering-writing/references/release-notes.md)
  and the fragment rules in the [release-notes index](docs/release-notes/README.md#writing-a-fragment).
  Each fragment's second line tags an area from the index; the collator decides whether to render it. The no-stability-promise policy lives only in that index. Before releasing,
  read the collated preview (`scripts/collate-release-notes`) end to end, merge
  entries on one topic, align tense and terms, and write `_intro.md` last.
  To release, run `scripts/collate-release-notes vX.Y.Z`, which writes
  `docs/release-notes/vX.Y.Z.md` and deletes the fragments but `_intro.md`;
  add the released version to the index and reset `_intro.md` for the next
  release.

### Specifications

- Owns: `non-derivable-intent`
- Drift gate: harness asset checks for retained runtime-graph projections;
  other prose is review-checked

### Merge policy

- Pointer: [`docs/worktrees.md`](docs/worktrees.md)

### Work-item runbook

- Pointer: [`docs/runbooks/github-workflow.md`](docs/runbooks/github-workflow.md)

This repository is loaded by multiple agent platforms. A convenient
Claude-only or Codex-only change is a regression unless the approved scope is
platform-specific.

Changes to the ambient `AGENTS.md` or `HARNESS.md` apply from the next session.
A live session retains the prior constitution until restart.

## Documentation tiers

Documentation claims sit in three tiers, and the third is deliberate:

1. **Code-checked.** Runtime schema and migration structure is owned by code
   and tests. Relative Markdown links in `docs/` and each `SKILL.md` are
   machine-checked.
2. **Review-checked.** The README skill count and architecture risk factors are
   maintained from current files and code; no harness check binds those regions
   to a source file. Specification prose, fenced commands, placeholders,
   unbound projections, skill-reference links and other links remain
   review-owned.
   `HARNESS.md` stays compact because it is read before cross-provider dispatch,
   risk-tier, authority or memory decisions; see [`AGENTS.md`](AGENTS.md).
3. **Unchecked prose.** Design intent and rationale carry no machine gate,
   on purpose; review keeps them honest, not tooling.

## Ambient doctrine rules

These rules keep the always-loaded files small. They are review-owned; the
Harness asset checks enforce the machine-checkable subset.

1. `AGENTS.md` and `HARNESS.md` are the only ambient doctrine owners.
   `AGENTS.md` stays minimal; `HARNESS.md` is the compact constitution for
   topology, lifecycle, user gates, risk, Git, routing and memory. A
   provider-bootstrap import shim, such as `CLAUDE.md`, may point only to
   those owners and adds no policy.
2. Operational depth has one skill owner: compaction and checkpoints in
   `session`, routing and degradation in `orchestrate`, and receipt detail in
   `deliver`. Ambient files name skills, then load depth only when triggered.
3. Nothing outside an owning skill names a file under its `references/`
   directory. Cross-skill references use the skill name only.
4. `AGENTS.md` and `HARNESS.md` contain no repository-relative `docs/`,
   `config/` or `scripts/` paths, with one deliberate exception: the
   `AGENTS.md` link to `docs/runbooks/shell-pitfalls.md`. Runnable commands are
   PATH-resolved. Fabric identity derives from the working directory.
5. `HARNESS.md` is the sole ambient skill resolver. It names the installed
   Claude and Codex skill roots; `AGENTS.md` names neither an installed root
   nor an instance skill directory. A named skill loads its `SKILL.md` and is
   binding, while provider-native discovery may implement resolution.
6. Ambient files carry no dates. Git records revision provenance.
7. `orchestrate` remains one skill. Skill changes use targeted,
   evidence-backed pruning; no new catalogue entry or duplicate policy is
   introduced without its own accepted scope and ownership boundary.
8. The repository process and GitHub mechanics live on repository-scoped
   surfaces. Durable decisions and specifications precede dependent
   implementation; `session` owns temporary handoff lifecycle.

## Change a skill

1. Confirm the capability belongs globally and is not better kept in a project.
2. Use `skill-craft` for a skill change: its audit branch for read-only
   assessment, its author branch for a new or materially revised skill under an
   explicit write envelope. `implement` owns end-to-end delivery and
   verification.
3. Use a consistent kebab-case capability name. Related writing skills use
   parallel names: `engineering-writing`, `natural-writing`, `legal-writing`.
4. Keep portable frontmatter to `name` and `description`. Keep required
   third-party notices with copied material and provider UI metadata in a
   validated sidecar. Metadata and tool lists may narrow invocation but never
   grant authority.
5. Put trigger terms and the nearest exclusion in the first 250 description
   characters. Keep the complete canonical catalogue at or below 8,000
   characters, targeting 7,600 for wrapper and version headroom.
6. Keep `SKILL.md` roughly 500 words or less. Move depth into narrowly named
   references loaded only when needed and deterministic behaviour into scripts.
7. Add positive, negative and boundary fixtures with exact primary and companion
   routes. Trigger fixtures are mandatory for every skill change;
   `scripts/check-skill-negatives` inventories the near-miss negatives under
   `skills/` and `library/`. A contract test is mandatory only where the change
   introduces or alters an invariant a script, validator or CI gate evaluates at
   runtime, so that violating it fails a command rather than a reviewer's
   reading. A change confined to wording,
   ordering, examples or explanation adds no contract test; the fixture set and
   the harness gates carry it. Assert parsed structure (fixture routes, JSON
   policy values, resolved paths) or a script's output, exit code or side
   effect. Never assert the text of a document. See `test-design` for the
   exclusion list.
8. A skill behaviour change runs its fixtures with and without the skill and
   records the difference for the reviewer; frozen comparative evals are
   conditional. See [skill-craft](skills/skill-craft/SKILL.md).
9. Re-run `scripts/check-harness` and the applicable targeted checks.

Split a skill when its triggers, artifacts or completion gates differ
meaningfully and a single-entrypoint branch selector cannot keep them
behaviourally separate at runtime. A branched skill may unify procedures of
differing authority under one frontmatter when each branch enforces its own
authority gate: for example an audit branch that can never write without an
explicit envelope naming the acting lifecycle owner. The runtime authority
boundary, not the file boundary, is what must hold. Merge skills when they
compete for the same request and lack a stable boundary. Retain only rules that
change behaviour or prevent observed failures.

Choose the smallest correct owner: an always-loaded project rule, occasional
skill, deterministic script or hook, external MCP or app capability, or stable
independently versioned plugin. Do not import popular packs wholesale. Extract
only licensed, evidence-backed mechanisms into the nearest owner; create a skill
only when its trigger, authority, artefact and gate remain distinct.

Nothing outside a skill may name a file under that skill's `references/`
directory. Cross-skill references use the skill name only. This is a review
rule; no contract test currently scans it. Required licence notices may name
the files they cover.

`natural-writing` is the writing hub and single owner of the shared prose
doctrine (mechanism catalogue, Australian-English house style, condense pass
and claim discipline) plus academic prose; `engineering-writing` and
`legal-writing` keep only their domain overlay and link back to the hub. Change
the shared doctrine in the hub, not in a domain skill. The hub's
`skills/natural-writing/lint-rules.json` owns the general and academic rule
vocabulary; `skills/legal-writing/lint-rules.json` owns legal rules and filing
blocker severities. Run `provenant check writing <paths> [--rules general|academic|legal]`.
Rules and exclusions are skill data, outside policy, configuration and records.

## Promote and retire

Promote a project skill when a clear reusable trigger, artifact, fixture and
ownership boundary justify it. When evidence is limited, start opt-in and
provisional. Generalise project-specific values into knobs, leave a thin local
override, and keep project rules authoritative inside their workspace.

Audit usage periodically. Retire zero-use skills that add no durable capability,
but preserve required third-party notices and use repository history instead of
live backup folders as the normal safety boundary.

The `provenant-config` crate owns source stamping, typed `release.json`,
immutable release installation and the managed-file receipt. Its library
supports dry-run previews, digest checks and removal of unchanged owned entries.
The Rust CLI ships `install`, `check install` and `uninstall`.

## Change the delivery kernel

Keep delivery invariants in `crates/workflow`. A new domain should compose the existing workflow
and a domain skill first. Add a workflow check or receipt kind only when its
artefacts, deterministic gates, judgement gates and release meaning are
materially distinct.

Trigger fixtures remain mandatory. Contract tests follow the
machine-enforced-invariant test in **Change a skill** step 7. The eval
triggers under **Change a skill** apply unchanged. When they fire here, use the
held-out portfolio ([`docs/evals/skill-portfolio-2026`](docs/evals/skill-portfolio-2026/README.md))
and lifecycle ([`docs/evals/lifecycle-routing-2026-07`](docs/evals/lifecycle-routing-2026-07/README.md))
datasets with repeated trials, recording raw
numerator and denominator, model and harness versions.

## Public and third-party hygiene

- Exclude personal absolute paths, private project names, credentials, local
  plugin caches, matter facts and private symlink targets.
- Import copyrightable expression only with a redistribution licence. Preserve
  applicable licence, copyright, notice and modification requirements. Ideas,
  methods, taxonomies, numeric thresholds and rubric weights do not by themselves
  require a third-party notice.
- Prefer source links and small adaptations to large generated bundles. Record
  why each third-party component is present.
- Treat plugins as supply-chain packages: before execution or installation,
  pin the source and ref, then inventory manifests, scripts, hooks, binaries,
  MCP or app endpoints, network and data flows, permissions, update and rollback,
  and component licences.
- Keep runtime examples synthetic and visibly placeholder-based.

## What a test may pin

- Pin externally visible behaviour, authority boundaries, and stable machine contracts.
- Prefer stable codes and focused fields over prose, full outputs, or incidental counts.
- Derive expected registries and lists from their source of truth.
- Keep security, confidentiality, evidence, lease, and schema assertions.
- When an unbreaking change fails a test, rewrite it to pin behaviour or delete it; never bend product code around it.

Three advisory test-strength signals run in `.github/workflows/test-signals.yml`.
They always exit 0, so they only warn:

- `scripts/assertion-diff` flags Rust test assertions that a diff removes or
  loosens.
- `scripts/run-mutants` mutation-tests the source lines a diff changed.
- `scripts/find-changed-tests` lists the Rust tests a change adds or alters.

## Verify and release

Run the checkout gates, or require exact-head `ci-status`:

For a pull request branch, run only focused Rust crate tests. Hosted CI runs the workspace gates on the exact PR head.

```sh
provenant check config
provenant check install
scripts/check-harness
provenant check release
git diff --check
```

The harness check requires Python 3 and PyYAML (`python3 -m pip install PyYAML==6.0.2`). It scans repository assets, not installed skill roots. Run `scripts/test-check-harness` to exercise its fixture tests.

Use `provenant check writing PATHS --rules general` for prose, with
`--rules academic` or `--rules legal` where they apply. Git pre-push
protection uses `provenant hook git pre-push REMOTE LOCATION`, and
`install --hooks on` owns hook registration. Follow the
[GitHub workflow](docs/runbooks/github-workflow.md) when preparing changes.

Each check returns ordered findings, exits 1 for findings, 2 for malformed
command input and 3 for unreadable evidence. Use `--limit N` and the returned
`--cursor TOKEN` for further pages. Changing inputs or rules invalidates a
cursor. Full test suites and release scans run in hosted CI.

Before publication, scan an exact nonempty ancestor range as well as the
tracked working files and raw HEAD. Pin both endpoints:

```sh
provenant check release \
  --base "$(git rev-parse --verify --end-of-options 'origin/main^{commit}')" \
  --head "$(git rev-parse --verify --end-of-options 'HEAD^{commit}')"
```

The check includes every selected commit's raw author, committer and message
bytes and every blob in its tree, including files added and deleted within
the range. It also checks publication heads for required legal files and
rejects private paths, symlinks, secrets, personal home paths and evidence
that cannot be read within the byte bounds. It reads no unrelated refs and
never fetches, publishes or rewrites history.

Publication used a fresh root commit, `a39c7b7e`. History rewrites stay gated
behind explicit user authority. Never push private pre-publication refs merely
because the current tree is clean.
