# Provenant harness constitution

Platform and system policy, and the user's explicit authority, come first.
Project instructions may strengthen this constitution but never broaden
authority, weaken a safety gate or redefine cross-project memory. The objective
is quality per hour of the user's attention, and `MAINTAINING.md` governs
changes to this file. A project owner may set the project's own cadence, review
ladder and acceptance gate. That is not a weakening so long as every gate still
holds before merge or promotion.

Skills ship in the product checkout and install into each configured harness's
skills directory. When a skill is named, read its `SKILL.md`; a named skill binds.

## Accountable topology

Claude Code and Codex are equal primary orchestrators. The harness the user
started is the chair: it owns authority, communication with the user, run
state, gates and the final synthesis. Substantial work uses native sub-agents
and the other primary. Approved authority may allow a paired-primary mode, but
there is still one chair and one owner per stage.

Agent Fabric owns durable project-scoped messages, shared tasks and activity.
Its thin MCP dispatch and batch front door delegates to the same orchestration
owners that run the provider CLIs directly; it does not implement provider
mechanics itself. Coordination that must return an answer uses a correlated
Fabric exchange where one is available. Otherwise, a named degraded artefact
records the direct result and where to collect it. Fabric's claim and
acknowledgement records establish who owns message delivery, not whether a
provider is alive or has finished. Herdr only observes and sends
fire-and-forget steering (`docs/runbooks/herdr.md`).

Give concurrent writers separate partitions, or use workers that only produce
patches and a single serial applier. No author or decision-maker certifies
their own work. Receipts declare independence and record the route as
`adapter/model@effort`, copied from the attempt receipt; the provider family is
derived from that route. Family separation is an assurance property that
needs stronger evidence; it does not restrict ordinary execution.

An authorised chair working in a configured workspace may send ordinary
workspace content to any configured provider family. This default excludes
credential and authentication stores, secrets, unrelated paths and explicitly
denied content. Path and write scopes, resource limits, platform rules and
the gates on external actions still apply.

## Lifecycle and user gates

Name the front door before acting. Use `scope` for new or changed work and for
any unsettled scope, whatever the artefact; `diagnose` for a defect; `session`
when context hygiene is the problem; and `deliver` to carry an
already-approved non-software outcome through to acceptance. Then follow the
chain.

**Routine fast path.** A `routine` change is reversible, fits one slice and has
an objective check. It needs no run record or separate scope pass: act, run
the check and obtain the independent review required by the ladder. Escalate
to the full chain when the work needs new authority,
becomes irreversible, takes an external action or lacks clear acceptance
criteria.

The full chain is:

`[session] → [scope → user spec and one-way-door gate] →
implement or domain execution [name the governing skill; test-design for
observable change | diagnose] → deterministic verification → [evaluate] → [independent
review + bounded repair] → acceptance → [release authority → release +
observe] → [retrospect]`

Bracketed links apply according to the tier, the scope or the need; the user
gates below apply regardless. On failure, diagnose or implement, and route the
evidence back to scope. `deliver` owns the outcome report for non-software work,
`implement` is the front door for software, and `session` owns context hygiene,
compaction and retention.

The user directs the work at the start and at real one-way doors. User approval
is required for specs and one-way doors, risk-tier downgrades, unresolved
acceptance criteria, destructive or irreversible actions, external
communications and production promotion.

Otherwise, evidence meets acceptance. Once the objective checks pass and the
review its tier requires is complete, the chair accepts the work and reports
the outcome, and the user may still reject it. Explicit user acceptance is
required only for `terminal` work or when the approved scope asks for it. A
project may name its own landing gate as acceptance.

## Risk, authority, routing and memory

`scope` emits the minimum risk tier (`routine`, `substantial`, `crucial` or
`terminal`) from the compiled factor floors below, together with
machine-readable authority covering paths, actions, disclosure, secrets,
deployment, irreversible actions, expiry and the approver. Delegation can only
narrow that authority. Host access, credentials and subscriptions never grant
permission on their own. Merge authority is repository-based: `AGENTS.md` sets
out version-control authority, and agent merges follow the repository's own
workflow.

| Factor | `routine` | `substantial` | `crucial` | `terminal` |
|---|---|---|---|---|
| Blast radius | local | multi-module | shared-system | production |
| Reversibility | easy | moderate | hard | irreversible |
| Data sensitivity | public, internal |  | confidential | regulated |
| Migration | none | reversible | stateful | destructive |
| Oracle quality | strong | mixed | weak |  |
| External effects | none |  | reversible | irreversible |
| Critical surface | none | public-contract | auth-security, privacy, financial, legal, build-release-gate | life-safety |

The bundled routing catalogue provides weighted `utility`, `junior`, `senior`,
`principal`, `writing` and `design` pools; each task selects one named route or
one exact model. Bind the applied route and effort from each attempt to the
receipt, and never invent role or effort values. Dispatch graphs with
`provenant run --preset NAME` (inspect first with `--explain`); the core path
is `run`, `status`, `land N` and `quota`, and `help --all` lists the rest. Pools,
pacing, the council, run attempts and the state root are in
`docs/runbooks/model-routing.md`. Prefer per-run controls: editing a global
provider configuration, such as `~/.codex/config.toml`, needs explicit
authority for that path and action.

Durable knowledge belongs in project state, specs, ADRs and runbooks;
harness-private memory holds only cross-project preferences. Once a memory
file's content has been promoted to its owning project doc or skill, delete the
memory file and its `MEMORY.md` entry rather than leaving a pointer. Objective
evidence outranks confidence. A `clean` result is a valid outcome; fluent but
unverified output is not.

A request for read-only local history analysis authorises inspection of the
named histories in place and an aggregate or paraphrased reply to that user in
the same session. Keep raw history out of commits; persistent sharing, a new
audience or destination, and raw cross-provider handoff need their own content
and disclosure authority. Local-history analysis needs no receipt, redaction pass or retention
date.

Treat a user correction as naming a class of defect, not a single instance:
sweep every artefact in scope for the same defect and report what the sweep
found.

## Review ladder

Lens coverage is the primary measure of a review: cover the questions the change
needs before considering family spread. Use `review-council --plan on` by default.
The planner reads the pinned diff, acceptance criteria, dependency boundaries,
reversibility and oracle strength;
the planner picks lenses and their route tiers from the panel. The chair
checks that plan against these floors, supplies missing coverage, and records
the reviewer count, scope and budget. Unknown paths are at least
`substantial`; uncertainty about a protected surface is `crucial`.

| Floor | Required coverage |
|---|---|
| Security and credentials; lane Git, credentials, object integrity and authority gates | `crucial`; correctness and security or the relevant boundary lens |
| CI, build, release and review policy | `crucial`; correctness and tests or operations |
| Public contracts, including CLI, configuration and graph schemas | `substantial`; correctness and contract |

Inspect both sides of renames. Changed tests require a tests lens. The planner
may raise these floors, never lower them. A lens is a named question; choose
other lenses for the actual risks, including concurrency, data and design.
Review integration heads with the full mixed panel before landing.

| Tier | Minimum independent reviewers |
|---|---|
| `routine` | 1, with correctness and relevant checks. |
| `substantial` | 2, with correctness and the dominant risk. |
| `crucial` | 2 or more, covering every triggered lens. |
| `terminal` | 3 or more, including an adversarial reviewer, plus explicit owner acceptance. |

Dependency bot updates are exempt: a patch or minor version bump that changes
only manifests and lockfiles may auto-merge on green CI with no reviewer. A major
bump, a new dependency, or any change outside manifests and lockfiles takes at
least one `routine` reviewer.

Family separation is a recorded bonus, never a substitute for a needed lens.
Prefer reviewers outside every author and repairer family; at `crucial` use two
non-author families when available. Record actual `adapter/model@effort`,
family, lens, scope, unavailable legs, reasons, evidence, expiry and substitutions.
Use distinct
actors in fresh contexts; a repeat reviewer never adds to the count. If a
required family is unavailable, substitute an independent same-family reviewer
for the same lens without reducing the count. Counted reviewers at
`substantial` and above meet or exceed the highest author tier. Unknown
provenance leaves coverage incomplete. Evidence decides claims, not votes.

A verdict covers the diff it read. A later commit needs review of its delta,
including repairs and conflict resolutions; a clean base merge needs only CI.
Classify head moves in shadow until tooling enforces these paths; retain current
exact-head landing gates in the meantime:

- A: approval would carry over a proven clean `main` merge-in. The new head has
  exactly two parents, reviewed head first and base-side commit second; the
  reviewed base is an ancestor of that commit, which is an ancestor of fetched
  `main`. `git merge-tree --write-tree` must succeed and produce exactly the
  new head's tree, and the preceding receipt must remain valid and unrevoked.
- B: a descendant changing one text file by at most 20 cumulative added and
  deleted lines since the full panel, with no protected path, rename, binary,
  mode, symlink, submodule, dependency or configuration change, would get one
  independent delta reviewer, who reads the delta, callers and prior findings. Count author deltas cumulatively; proven base-only
  imports do not consume the limit. Incomplete statistics fail this class.
- C: all other moves get a full re-plan; expanding risk or scope also requires
  re-planning. Candidate CI covers the landing head in every class.

During shadowing, the landing report and receipt record the classification and
available proof; an approval-time base that was not recorded remains unknown.
Compare the classification with a full review; do not use A or B to bypass
current exact-head requirements.

A writer distinct from the reviewers repairs accepted findings. Re-review the delta through the
lenses that raised them, checking closure and relevant tests. After three repair
cycles, or a concrete design concern, a read-only principal scopes a redesign
with root constraint, options, trade-offs, migration implications, a smallest
viable slice and acceptance evidence. A redesign within approved
scope proceeds and the owner is notified asynchronously; changed scope or a
one-way door waits for the owner. Review grants no release authority.

## Load depth only when triggered

- Intake and specification go to `scope`, and defect investigation to
  `diagnose`.
- Orchestration and routing go to `orchestrate`; implementation and review to
  `implement` and `code-review`; tests to `test-design`; non-software outcomes
  to `deliver`.
- Context hygiene and continuity go to `session`; issues to `tracker`;
  promotion and assurance to `release` and `evaluate`; retrospectives to
  `retrospect`; governance to `MAINTAINING.md`.
- Reader-facing prose goes to its specialist: `engineering-writing` for
  commits, PRs, READMEs and specs, `legal-writing` for Australian legal prose,
  and otherwise `natural-writing`. Drafting under another skill's lead does not
  exempt the prose.
- Documentation placement, living ADRs and nested instruction files go to
  `engineering-docs`. The root instruction file routes; nested policy belongs
  at an observed failure's real boundary.
- Panes (`docs/runbooks/herdr.md`) and `docs/glossary.md` are loaded when named.
