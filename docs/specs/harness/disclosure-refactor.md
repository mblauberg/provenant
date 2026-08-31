# Spec: harness progressive-disclosure refactor

Canonical decision specification. Git history retains revision and review
provenance; `.agent-run/` artifacts are not normative dependencies.

Canonical decision: [ADR 0020](../../adr/0020-retire-the-daemon-fabric.md) owns
the current daemonless Fabric wording; the progressive-disclosure decisions
remain normative.

## Problem

The ambient instruction layer is heavier than its job requires and leaks
repo-relative paths and skill internals:

- `AGENTS.md` (~34 lines) is loaded every prompt on both harnesses (symlinked
  `~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`). `HARNESS.md` (~113 lines,
  ~1.4k tokens) is read on a broad trigger and cascades into 3 orchestrate
  reference files plus runbooks — a ~4–6k-token effective constitution read.
- `HARNESS.md` mixes constitution (authority, gates, review pressure) with
  operational depth owned elsewhere: compaction cadence (`session`), routing
  mechanics (`orchestrate`), receipt detail (`deliver`).
- Files outside a skill reach into `skills/<x>/references/*`: `HARNESS.md` →
  3 orchestrate references; accelerator workflows (`~/.claude/workflows/*.js`)
  → `dynamic-workflows.md`, `cli-headless.md`; writing skills →
  `natural-writing/references/*`.
- `AGENTS.md`/`HARNESS.md` carry 9 repo-relative `docs/*`, `config/*`,
  `scripts/*` paths that mislead agents whose cwd is another repository.

`docs/ARCHITECTURE.md` already states the intended model — tiny bootstrap,
compact constitution, skills load depth only when triggered — so this
refactor enforces existing doctrine.

## Decisions (user-approved 2026-07-20; D-numbering stable across revisions)

| # | Decision |
|---|---|
| D1 | Two-file ambient layer, both stripped. `AGENTS.md` ≤ 35 lines. `HARNESS.md` becomes pure constitution ≤ 60 lines: accountable topology, lifecycle + user gates, risk tiers + review-pressure table, standing git envelope, 2-line routing invariant, 1-line memory rule, trigger index. These two caps are hard static gates. |
| D2 | Strip destinations: compaction/checkpoint cadence → `session`; routing depth/degradation → `orchestrate`; receipt schema detail → `deliver`. Each already owns a landing reference. |
| D3 | Cross-skill reference rule: nothing outside skill X names a file under `skills/X/references/`. Cross-references use the **skill name only** (e.g. "`implement` skill"), not paths. Enforced by a contract test. Writing-family links to `natural-writing` internals are rewritten to skill-name references; the hub keeps owning shared prose doctrine. |
| D4 | No repo-relative paths in `AGENTS.md`/`HARNESS.md`. Repo-local process pointers move to repo-scoped surfaces or become skill-name references. Runnable commands are PATH-resolved (`provenant …`), never location-bearing; Fabric identity is derived from the working directory and needs no workspace-trust command. |
| D5 | Orchestrate stays one skill; per-file verdicts in the table below. No new catalogue entries (see AC-S5 baseline). |
| D6 | MAINTAINING.md: frozen held-out eval comparisons downgrade to on-suspicion/pre-publication; trigger fixtures + contract tests stay mandatory. Recorded as ADR-0014. |
| D7 | Content depth: structure + targeted pruning (evidence-backed deletions only; no wholesale prose rewrites). |
| D8 | Orchestrate verdict table approved (r1, 2026-07-20; F2 evidence amendment folded). Removed files archive to `docs/research/` (with index entry) rather than delete; `debate-and-panels.md` is the one approved merge-then-delete because its rules survive verbatim inside `verification.md`. |
| D9 | The "GitHub (this repo only)" bullet moves out of global `AGENTS.md`; its authority clause ("agent merges authorised") lands in `MAINTAINING.md`, mechanics stay in the github-workflow runbook. |
| D10 | `AGENTS.md` and `HARNESS.md` carry no dates; revision provenance lives in git history. |
| D11 | **Source drift.** This spec binds decisions, not file states. Implementation re-validates source-owned evidence at the accepted issue's head; Git history preserves provenance. A source change that contradicts a decision re-opens that decision, not the interview. |
| D12 | **Skill-name resolution contract.** Each ambient file carries one resolver line: skills live at `$HOME/.agents/skills/<name>/` and a named skill means read its `SKILL.md`, which discloses its own references. Claude resolves via its Skill tool; Codex resolves via the resolver line + installed `~/.codex/skills/` mirror. The resolver line is the sole location-bearing statement in the ambient files. Skill names are binding, not advisory. |
| D13 | **Acceptance structure.** Acceptance splits into (a) static gates — machine-checkable on the final tree; (b) per-PR checks — run in each PR's CI; (c) release conditions — history/process states confirmed at close-out. The two ambient line caps (D1) are hard maxima; reference-file budgets are advisory targets paired with named retained-content invariants (r3 amendment: line counts alone invite formatting games and fail legitimate edits). |
| D14 | **Accelerator custody (r3, user-approved).** Canonical sources for `cross-verify.js`, `codebase-polish.js`, `implement-run.js` move into the repository under `workflows/`; `install-harness` manages `~/.claude/workflows/` from them like it manages skills. They are a Claude-only surface (they drive Claude Code's Workflow tool); Codex does not consume them and reaches equivalent orchestration through the `orchestrate` skill. Once in-tree they fall under the D3 reference rule and AC-S2 scan. Includes a bounded alignment refresh: re-validate their doctrine citations against post-prune reality; behavioural redesign stays out of scope. |
| D15 | **Documentation custody (r3, user-approved).** A landing PR (PR0) commits this spec, the execution handoff, ADR-0014, and the `docs/specs/` + `docs/handoffs/` index entries before implementation PRs begin. ADR-0014 lands here instead of PR4; PR4 keeps the MAINTAINING.md amendments that cite it. The handoff and its index entry are removed by the session that consumes them. |

## Migration manifest (single fixture; replaces the separate disclosure-ledger fixture and verdict manifest)

One machine-readable fixture at `tests/fixtures/disclosure-migration.yaml`
(schema `disclosure-migration.v1`) records both migration inventories. Current
tests consume selected surviving invariants; the fixture is not a live exact-tree
manifest. Rows below are the approved content.

Ambient rows (`section`, `disposition`, `destination` — owner is a skill
unless marked repo-surface; repo-surfaces sit outside AC-S3's "exactly one
owning skill" count):

| Source section | Disposition | Destination |
|---|---|---|
| HARNESS preamble: authority hierarchy | retain (condensed; date + doc paths removed) | — |
| Topology: chair/equal-primary, writer partition, author≠certifier | retain core | pairing/pane depth → `orchestrate` |
| Lifecycle map + mandatory user-approval list | retain condensed | profile/receipt detail → `deliver` |
| Risk tiers + standing git/worktree envelope | retain | runbook pointers → repo-surface (MAINTAINING.md) |
| Routing paragraph | retain 2-line invariant | mechanics/degradation → `orchestrate` |
| Provider-controls/compaction cadence | strip | `session` |
| Review-pressure table | retain | — |
| Context/evidence: memory rule, evidence-over-confidence | retain 2 lines | receipt schema → `deliver` |
| Trigger index | retain as skill-name index | — |
| AGENTS: objective, sub-agents, memory, git, Fabric trust, CLI, style bullets | retain (path-free per D4) | — |
| AGENTS: GitHub (this repo only) bullet | strip | repo-surface (MAINTAINING.md authority clause + runbook mechanics) |
| AGENTS/HARNESS: all dates | remove (D10) | git history |

Orchestrate rows (17 files; evidence: r1 subagent audit, all files read,
consumers grepped repo-wide plus `~/.claude/workflows/*.js`; F2 amendment —
`system-design-patterns.md` has exactly one live consumer, its loader entry in
orchestrate `SKILL.md`; verdict unchanged):

| File | Verdict | Retained-content invariants / notes |
|---|---|---|
| trigger-boundary.md | keep | distinct territory |
| routing-and-tiers.md | keep | becomes canonical home of "route by role/tier, never memorised model IDs" (currently triplicated) |
| codex-subagents.md | keep | contract-tested Codex adapter |
| orchestration-contract.md | keep | the declared public interface; dedup target |
| dynamic-workflows.md | slim (advisory ~60 lines) | must retain: Workflow contract binding, saved-workflow conventions, pointers; drop restated native Workflow docs and the run-dir list duplicated from memory-scratchpad |
| paired-primary.md | keep | replace its Fabric-vs-Herdr restatement with a pointer |
| herdr-panes.md | keep | third-party CLI contract, capability-tested |
| layering-and-context.md | keep | canonical for worker caps (3–5) |
| retrieval-and-tool-routing.md | keep | worker-brief block becomes a pointer to orchestration-contract's worker contract |
| verification.md | keep | absorbs debate-and-panels content |
| debate-and-panels.md | merge into verification.md, then delete | the approved non-archive removal (D8) |
| domain-adaptation.md | keep | distinct |
| system-design-patterns.md | archive to `docs/research/` | orphaned survey; loader entry removed; research index entry added |
| evaluation-and-observability.md | keep | distinct angle |
| memory-scratchpad.md | keep | canonical run-dir schema |
| worker-liveness.md | keep | read-only worker liveness guidance and threshold owner |
| cli-headless.md | keep | routing section becomes a pointer; load-bearing for cross-verify |
| autonomous-implementation.md | keep | consumed by autopilot |

Consumer migration, single change with the prune:

1. Remove loader entries for archived/merged files from orchestrate SKILL.md.
2. Repair `verification.md:49` forwarding pointer before deleting
   `debate-and-panels.md`.
3. Update `evals/contract_cases.yaml:40,54` reference invariants.
4. Replace the hand-maintained `REQUIRED_REFS`
   (the former trigger checker) with a set **derived from this manifest's
   keep+slim rows** — never from the directory listing itself (a
   directory-derived expectation is circular and cannot detect accidental
   deletion).
5. Update `docs/research/README.md`: archived files get an index entry with a
   normative-owner note, per that index's own contract.
6. Accelerator repoints (private-path citations → public
   `orchestration-contract` doctrine or embedded rule) happen in PR5 against
   the in-repo sources (D14); never leave a dangling private-path citation.

## Acceptance

Current gates and retained migration conditions:

- AC-S1: `AGENTS.md` ≤ 35 lines; `HARNESS.md` ≤ 60 lines (hard, D1); no
  dates; no repo-relative `docs/`/`config/`/`scripts/` paths; no
  `skills/<x>/references/` paths anywhere outside the owning skill;
  cross-references by skill name only. The D12 resolver line is the sole
  location-bearing statement; PATH-resolved `provenant` invocations are not
  location-bearing. Scan counts prose and code/comment content alike.
- AC-S2: reference-rule contract test passes. Scan scope (in-tree only):
  skills, ambient files, `scripts/`, `workflows/` (post-D14), live
  tests/fixtures (updated in the same change). Declared allowlist:
  `docs/archive/`, `docs/research/`, `.agent-run/`, git history.
- AC-S3/AC-S4 were migration acceptance conditions. The retained fixture feeds
  selected owner-specific tests; it is not a permanent full-tree checker.
- AC-S5: catalogue within the approved cap, reviewed against the source
  catalogue. The former generated-catalogue measurement is historical and is
  not a current gate.

Per-PR checks:

- AC-P1: `scripts/check-harness`, `scripts/static-security-check.py`,
  `scripts/public-release-check`, `git diff --check` green, plus the PR's own
  focused tests.
- AC-P2: installer fixtures green in isolated temp homes for the full matrix
  {`--platform claude`, `--platform codex`} × {clean install, upgrade over a
  harness-managed file, existing-unmanaged-instructions branch}. Oracles per
  cell: expected exit code (0 / 0 / 3), expected link/manifest state, and —
  on the exit-3 arm — byte-identical preservation of the unmanaged file
  (`install-harness:113-129`, extending `test_install_harness.py:184-195`).
- AC-P3: skill-resolution fixture green — from an isolated install, every
  skill name referenced in the ambient files resolves to an installed
  `skills/<name>/SKILL.md` on both platform layouts (including the
  `~/.codex/skills/` mirror), and the resolver line's stated root matches the
  installed root. This is a static install/discovery contract; live
  model-routing behaviour is explicitly out of scope (ADR-0014
  detection-in-use applies).

## Current ownership

The repository's declared issue tracker owns change scope and stories. This
specification retains progressive-disclosure invariants and acceptance
requirements; it does not report delivery status, implementation history or
current work state. Durable decisions remain in ADRs and runtime structure
remains owned by code and tests.
