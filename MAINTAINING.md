# Maintaining Provenant

Read [AGENTS.md](AGENTS.md) first. This file holds the repository's process
declarations and the release steps. [ADR 0035](docs/adr/0035-three-hard-boundaries-and-flexible-everything-else.md)
defines the hard boundaries. Preserve unrelated work.

## Repository process

### Tracker

- Choice: `github-issues`
- Pointer: <https://github.com/mblauberg/provenant/issues>

### Scope and stories

- Canonical home: issue-tracker
- Pointer: <https://github.com/mblauberg/provenant/issues>

Scope and stories live in the parent issue; project docs retain durable intent,
decisions and invariants.

### Workflow state

- Owner: tracker
- Pointer: <https://github.com/mblauberg/provenant/projects/2>

### Docs layout

- Pointer: [`docs/`](docs/)

### Change history

- Owns: `git log` for what changed and when; [ADRs](docs/adr/) for why

Edit living docs in place. Each user-visible change adds a fragment under
[release notes](docs/release-notes/next.d/), following the
[fragment rules](docs/release-notes/README.md#writing-a-fragment).
`scripts/collate-release-notes --check` validates them in CI. Before a release,
read the collated preview (`scripts/collate-release-notes`), merge related
entries, align terms and write `_intro.md` last; `scripts/prepare-release` then
collates and consumes the fragments, and you reset `_intro.md` for the next
release. GitHub Releases holds published history.

### Specifications

- Owns: `non-derivable-intent`
- Drift gate: `scripts/check-doc-truth` (CI, and weekly through the docs-drift
  workflow) checks spec and runbook headers, anchors and indexes, commands and
  flags against `provenant help --all`, named error codes, variables and
  configuration keys against the sources and `provenant config schema`, and
  `Spec: docs/specs/NAME.md#anchor` comments in Rust. Specs name owning
  symbols, never individual tests, line numbers or inventory counts. Other
  prose is review-checked.

### Merge policy

- Pointer: [`docs/runbooks/github-workflow.md#merge`](docs/runbooks/github-workflow.md#merge);
  worktree custody and cleanup are in [`docs/worktrees.md`](docs/worktrees.md)

### Work-item runbook

- Pointer: [`docs/runbooks/github-workflow.md`](docs/runbooks/github-workflow.md)

## How Provenant is built

The [deliver skill](skills/deliver/SKILL.md) owns the engineering stance and
[test-design](skills/test-design/SKILL.md) the test invariants. Checks allow or
warn by default; hard holds protect only the three boundaries.

## Documentation and skills

Code and schemas own implemented fields, flags and defaults
(`provenant help --all`, `provenant config schema`); specs own non-derivable
invariants, runbooks procedures and ADRs decisions. Use
[engineering-docs](skills/engineering-docs/SKILL.md) for placement and
[improve](skills/improve/SKILL.md) for skill changes.

`scripts/check-harness` checks ambient doctrine and installed skills.
`GLOBAL-AGENTS.md` is the ambient source, installed as each harness's user-level
`AGENTS.md`; `CLAUDE.md` holds only `@AGENTS.md`. Ambient rules avoid
repository paths and dates, nothing outside a skill names a file under its
`references/` directory, and installed skill links stay inside that skill.
Platform-specific scope is explicit. Ambient changes apply to new sessions.

## Verify and release

Start a release with `scripts/prepare-release vX.Y.Z`; its header lists what it
does and it prints the remaining steps. Review and commit its changes, then run
the gates, or require exact-head CI evidence ([CI runbook](docs/runbooks/ci.md),
[test tiers](docs/runbooks/test-tiers.md), [release skill](skills/release/SKILL.md)):

```sh
provenant check config
provenant check install
scripts/check-harness
provenant check release
git diff --check
```

Before publication, scan the exact nonempty ancestor range as well as the
working tree and HEAD:

```sh
provenant check release \
  --base "$(git rev-parse --verify --end-of-options 'origin/main^{commit}')" \
  --head "$(git rev-parse --verify --end-of-options 'HEAD^{commit}')"
```

Notes with breaking changes never ship in a patch; the
[consumer contract](docs/specs/consumer-contract.md#stability-tiers) sets which
tier changes in which release, and `scripts/collate-release-notes` warns on a
patch cut. Bump `LIVE_RECORD_FORMAT` in
`tools/front-door/crates/records/src/install.rs` on any release that changes
files a live attempt's supervisor or settlement reads, so install can warn
before swapping under live lanes.

Keep secrets, private paths and inappropriate third-party material out of
published files and history. Follow the [GitHub workflow](docs/runbooks/github-workflow.md)
for review and landing.
