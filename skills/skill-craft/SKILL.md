---
name: skill-craft
description: "Use to audit an Agent Skill's triggers, overlap, token cost and fixtures (read-only), or to create or materially revise one under an authority grant. Not for delivering the change (implement) or measuring model quality (evaluate)."
---

# Skill craft

A skill is a reusable, triggerable procedure. Judge it on four pillars:
**trigger** (which requests load it, and which must not), **structure**
(contracts and progressive disclosure), **steering** (the few bright-line rules
a fresh agent would otherwise miss) and **pruning** (sediment, duplication,
sprawl and no-ops).

Follow the target repository's authoring rules in its `AGENTS.md`,
`CONTRIBUTING` and runbooks, where present. Keep project policy there rather
than copying it into this skill.

## Pick a branch

- **Audit (default).** Read-only assessment of an existing skill. Follow
  [references/audit.md](references/audit.md).
- **Author.** A new skill, or a material revision, under an authority grant
  that names `implement` as the action owner. Follow
  [references/author.md](references/author.md).

With no authoring request, audit. An audit that finds defects reports them. It
does not fix them without the grant.

## How it runs

- Audit several skills with `provenant run --preset fan-out-synthesise
  --set 'parameters.width=3' --prompt-file B.md --explain`: one read-only lane
  per skill, then synthesis. Submit only after `--explain` looks right.
- Rewrite several skills with `provenant run --preset big-batch --params-file
  lanes.toml --explain`, one `lanes` entry per skill. Prepare each writer
  worktree first; `big-batch` does not provision them.

## Working rules

- **Baseline first.** Run the target task without the skill, or with the
  previous version. Keep only the lines the baseline gets wrong, and delete any
  line it already satisfies. Record the with-and-without difference as reviewer
  evidence. Scores are advisory.
- **Name failures by mechanism:** premature completion, duplication, sediment,
  sprawl, no-op or negation-only.
- **Front-load the description.** Put the trigger terms and nearest exclusion
  early. The exclusion names the skill to use
  instead, because a bare negation does not route.
- **Fixtures.** Include near-miss negatives whose expected routes omit the
  target skill. Re-run them after any routing-text change, using the target
  repository's fixture policy and tooling where provided.
- **Contract tests** only for invariants a script or CI gate enforces. Never
  assert wording.
- **Dated versions.** Version facts go in a dated reference.
- **Hard rules in SKILL.md.** Load depth from references one hop away, and
  load it late.
- **Write rules** as mechanism, reason and example, phrased positively.
- **Project skills** that only point at a global skill are redundant. A
  project skill keeps only the repo judgement no doc holds, states its own hard
  rules, and lists each departure under `Project overrides:`.
- **Provenance.** Copied or closely paraphrased third-party text needs its
  source URL, commit, retrieval date and licence, with the notice kept.

## Evidence

Prefer 2026 sources and mark older ones as possibly stale. The dated research
basis is in [references/audit.md](references/audit.md#research-basis).
