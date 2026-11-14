# Maintaining Provenant

Read [AGENTS.md](AGENTS.md), [GLOBAL-AGENTS.md](GLOBAL-AGENTS.md) and the
[architecture](docs/ARCHITECTURE.md). The global rules own authority and
boundaries; [worktrees](docs/worktrees.md) owns Git isolation and operations.
Preserve unrelated work and follow the repository process below.

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
[fragment rules](docs/release-notes/README.md#writing-a-fragment). Before a
release, read the collated preview, merge related entries, align terms and
write `_intro.md` last. Run `scripts/collate-release-notes vX.Y.Z`, then reset
`_intro.md` for the next release. GitHub Releases holds published history.

### Specifications

- Owns: `non-derivable-intent`
- Drift gate: `scripts/check-doc-truth` checks spec indexes, headers, anchors,
  citations and diagram metadata; other prose is review-checked.

### Merge policy

- Pointer: [`docs/worktrees.md`](docs/worktrees.md)

### Work-item runbook

- Pointer: [`docs/runbooks/github-workflow.md`](docs/runbooks/github-workflow.md)

## How Provenant is built

The [deliver skill](skills/deliver/SKILL.md) owns the engineering stance.

- Fix root causes and the class of defect; prefer long-term structure to patches.
- Build extensible, configurable, modular mechanisms with one declared core
  and one owner per concern. Generalise where it makes change easier.
- Use explicit typed failures and catch mistakes before runtime. Checks allow or
  warn by default; hard holds protect credentials, Git isolation, data integrity
  and authority. When busy, skip and retry.
- Move callers to the new shape and remove compatibility code. Tests pin
  invariants derived from declarations; see [test-design](skills/test-design/SKILL.md).

## Documentation and skills

Use [engineering-docs](skills/engineering-docs/SKILL.md) for placement and
[improve](skills/improve/SKILL.md) for skill changes. Code and schemas own
implemented fields and defaults; specs own non-derivable invariants, runbooks
own procedures, and ADRs own decisions and reasons.

The harness checks ambient doctrine and installed skills. `GLOBAL-AGENTS.md`
is the ambient source, installed as `AGENTS.md`; provider import shims add no
policy. Nothing outside a skill names a file under its `references/` directory.
Installed skill links stay inside that skill. Ambient rules avoid repository
paths and dates; Git records provenance.

This repository supports multiple agent platforms. Platform-specific scope
must be explicit. Changes to ambient rules apply to new sessions.

## Verify and release

Use the [CI runbook](docs/runbooks/ci.md), [test tiers](docs/runbooks/test-tiers.md)
and [release skill](skills/release/SKILL.md) for the relevant checks. Run the
checkout gates or require exact-head CI evidence:

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

Keep secrets, private paths and inappropriate third-party material out of
published files and history. Follow the [GitHub workflow](docs/runbooks/github-workflow.md)
for review and landing.
