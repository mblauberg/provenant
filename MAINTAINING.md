# Maintaining Provenant

Read [`AGENTS.md`](AGENTS.md), [`HARNESS.md`](HARNESS.md),
[`docs/ASRS.md`](docs/ASRS.md) and
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) before changing the harness.
Inspect the live diff and preserve unrelated work. `HARNESS.md` owns lifecycle,
risk, authority, orchestration and review policy; [`docs/worktrees.md`](docs/worktrees.md)
owns branch and linked-worktree operation. Use the
[GitHub work-item workflow](docs/runbooks/github-workflow.md) for issue intake,
Project status, pull-request linking and user gates.

Agent merges are authorised for this repository; mechanics remain in the
GitHub work-item workflow. This is Provenant-local process, not harness
doctrine.

This repository is loaded by multiple agent platforms. A convenient
Claude-only or Codex-only change is a regression unless the approved scope is
platform-specific.

Changes to the ambient `AGENTS.md` or `HARNESS.md` apply from the next session.
A live session retains the prior constitution until restart.

## Add mechanism only in proportion

[`docs/ASRS.md`](docs/ASRS.md) and
[ADR 0021](docs/adr/0021-proportionate-mechanism.md) govern every change that
adds a check, gate, guard, abstraction or configuration surface. Answer its
four questions in the pull-request description: which failure has actually
occurred, who the adversary is, what the cheapest thing that detects it is, and
what the mechanism costs when it is itself wrong. A change that cannot name a
failure and an adversary other than the operator's own environment does not
land.

The same document owns the removal test, whose default outcome is deletion.
Apply it whenever auditing, not only when a file looks suspicious: code with no
caller outside its own tests, a script no document tells an agent to run, and a
rule already enforced by a shared helper are all deletion candidates. Preserve
the tip on an archive branch and say so in the change description.

Two exemptions, both narrow. The guarantees listed under "What is not
negotiable" are not simplified away; only their mechanism is ever in question.
Documentation and tests carry no such burden, because a wrong comment is cheap
and a wrong gate blocks work.

## Documentation tiers

Documentation claims sit in three tiers, and the third is deliberate:

1. **Source-owned.** Marked regions in the README and
   `docs/ARCHITECTURE.md` remain maintained documentation, with their source
   constants and prose reviewed together. Do not hand-edit generated-looking
   content without checking the corresponding source.
2. **Review-checked.** The spec schema baseline and migration assertions have
   retained machine checks. Table prose, fenced commands, placeholders,
   relative links and document projections remain review-owned.
   `HARNESS.md` stays compact because it is an always-loaded constitution.
3. **Unchecked prose.** Design intent and rationale carry no machine gate,
   on purpose; review keeps them honest, not tooling.

## Change a skill

1. Confirm the capability belongs globally and is not better kept in a project.
2. Use `skill-craft` for a skill change: its audit branch for read-only
   assessment, its author branch for a new or materially revised skill under an
   explicit write envelope. `implement` owns end-to-end delivery and
   verification.
3. Use a consistent kebab-case capability name. Related writing skills use
   parallel names: `engineering-writing`, `academic-writing`, `legal-writing`.
4. Keep portable frontmatter to `name` and `description`. Put provenance in a
   notice and provider UI metadata in a validated sidecar. Metadata and tool
   lists may narrow invocation but never grant authority.
5. Put trigger terms and the nearest exclusion in the first 250 description
   characters. Keep the complete canonical catalogue at or below 8,000
   characters, targeting 7,600 for wrapper and version headroom.
6. Keep `SKILL.md` roughly 500 words or less. Move depth into narrowly named
   references loaded only when needed and deterministic behaviour into scripts.
7. Add positive, negative and boundary fixtures with exact primary and companion
   routes, plus contract tests for machine-enforceable invariants. Trigger
   fixtures and contract tests remain mandatory for every skill change.
8. Under [ADR 0014](docs/adr/0014-comparative-skill-evals-on-suspicion.md),
   frozen held-out comparative evals are conditional. Run them when a routing
   regression is suspected or observed in use; a change rewrites
   trigger-bearing description text across several skills, at the maintainer's
   discretion; or the harness is being prepared for publication or another
   operator. When run, freeze held-out cases and compare candidate,
   without-skill and previous-package arms on current primary families. Retain
   invalid, omitted, timed-out and failed attempts with model lineage. Routine
   skill maintenance loses a heavyweight gate; the operator accepts
   detection-in-use as the routing-regression backstop. Publication to other
   users re-arms the full requirement; this ADR must be revisited in any
   public-release checklist.
9. Re-run the public-safety and full harness gates.

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
only when its trigger, authority, artifact and gate remain distinct.

Nothing outside a skill may name a file under that skill's `references/`
directory. Cross-skill references use the skill name only. The harness contract
test enforces this for the ambient `AGENTS.md` and `HARNESS.md`; elsewhere it is
a review rule. Licence attribution in `THIRD_PARTY_NOTICES.md` is the deliberate
exception, because an attribution must name the file it covers.

`natural-writing` is the writing hub and single owner of the shared prose
doctrine (tiered anti-AI taxonomy, Australian-English house style, condense pass
and claim discipline); `engineering-writing`, `academic-writing` and
`legal-writing` keep only their domain overlay and link back to the hub. Change
the shared doctrine in the hub, not in a domain skill. The hub's
`skills/natural-writing/scripts/style_lint.py` owns the shared lint vocabulary
the domain linters import, so a change there ripples to all of them.

## Promote and retire

A project skill earns global promotion after proving useful in at least two
projects. Generalise project-specific values into knobs and leave a thin local
override. Project rules stay authoritative inside their workspace.

Audit usage periodically. Retire zero-use skills that add no durable capability,
but preserve required third-party notices and use repository history instead of
live backup folders as the normal safety boundary.

Record a public rename in `config/skill-renames.json`. Test the managed
reconciliation path; do not rely on users deleting or replacing global links by
hand. Run `scripts/manage_installation.py plan --target <skills-dir>`, then
`reconcile --target <skills-dir> --renames config/skill-renames.json`. Ordinary
installation does not apply the rename registry.
Never claim or overwrite an unmanaged target.

## Change the delivery kernel

Keep profile policy in `config/delivery-profiles.json`, surface-selected checks
in `config/security-evidence.json` and machine invariants in the `deliver`
validator. A new domain should compose an existing base profile and a domain
skill first. Add a base profile only when its artifacts, deterministic gates,
judgement gates and release meaning are materially distinct.

Trigger fixtures and contract tests remain mandatory. Under
[ADR 0014](docs/adr/0014-comparative-skill-evals-on-suspicion.md), frozen
held-out comparative evals are conditional. Run them when a routing regression
is suspected or observed in use; a change rewrites trigger-bearing description
text across several skills, at the maintainer's discretion; or the harness is
being prepared for publication or another operator. When run, use the held-out
portfolio and lifecycle dataset with repeated trials, recording raw numerator
and denominator, model and harness versions. Routine skill maintenance loses a
heavyweight gate; the operator accepts detection-in-use as the
routing-regression backstop. Publication to other users re-arms the full
requirement; this ADR must be revisited in any public-release checklist.

## Public and third-party hygiene

- Exclude personal absolute paths, private project names, credentials, local
  plugin caches, matter facts and private symlink targets.
- Import material only with a redistribution licence. Preserve upstream
  licence, copyright, notice and modification requirements beside it.
- Prefer source links and small adaptations to large generated bundles. Record
  why each third-party component is present.
- Treat plugins as supply-chain packages: before execution or installation,
  pin the source and ref, then inventory manifests, scripts, hooks, binaries,
  MCP or app endpoints, network and data flows, permissions, update and rollback,
  and component licences.
- Keep runtime examples synthetic and visibly placeholder-based.

## Verify and release

Run the checkout gates, or require exact-head `ci-status`:

```sh
scripts/check-harness
npm run check
node runtime/fabric/mcp-smoke.mjs
npm audit --workspace=@local/fabric --omit=dev --audit-level=high
git diff --check
```

`check-harness` runs `static-security-check.py` and the default public-tree scan
itself, so the `ci-status` alternative above is a real equivalent rather than a
shorter list. The history and publication-range scans below are not in the gate:
they need refs a shallow CI checkout does not fetch.

Audit every ref reachable in the local clone with:

```sh
scripts/public-release-check --history
```

Before a public push, prove the exact non-empty commit range selected for
publication. `origin/main` must be an ancestor of `HEAD`:

```sh
scripts/public-release-check --publication-range \
  "$(git rev-parse --verify --end-of-options 'origin/main^{commit}')" \
  "$(git rev-parse --verify --end-of-options 'HEAD^{commit}')"
```

The publication-range check applies the public-tree policy to the selected
`HEAD` and scans the selected commits, their trees, messages and author email.
It is target-scoped evidence: it deliberately ignores the checkout, index and
unrelated private refs. The script owns the hardened raw-object verification
details; default no-flag mode still checks the checkout and index.

Publication used a fresh root commit, `a39c7b7e`. History rewrites stay gated
behind explicit user authority. Never push private pre-publication refs merely
because the current tree is clean.
