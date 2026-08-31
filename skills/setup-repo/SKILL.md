---
name: setup-repo
description: "Use when asked to set up or scaffold a repository's process, labels, tracker, branch ruleset, issue forms, docs layout, work-item runbook, or board. Not for this repo's own day-to-day GitHub mechanics; use its runbook."
---

# Setup

## User gate first

Before any write, user approves a plan naming the target, the exact
keep/create/adapt/conflict diff and, for GitHub, the Project board. Confirm
permission. Ask for an unnamed target; never infer it from cwd.

## Inspect and classify

Read the target's `Repository process` declaration first, then inventory
`MAINTAINING.md`, docs layout, `.github/**`, the work-item runbook and board.
Classify each action as **keep (exact match)**, **create
(absent)**, **adapt (compatible; propose the merge)** or **conflict (semantic
mismatch; STOP and ask)**. Never overwrite; amend the declarations block per
heading. A re-run against an already-set-up repository must produce no diff.

## Choose tracker

Ask: **Use GitHub issues?** If yes, use the GitHub branch, confirming
the remote host before any `gh` command. If no, record the chosen tracker or
`none`; skip labels, ruleset, issue forms and board. Tracker-specific setup
remains out of scope: a documented skip, not alternate-tracker scaffolding.

## Declare repository process

Amend the fixed-heading **Repository process** block in `MAINTAINING.md`,
creating it if absent, from
[`templates/repo-declarations.md`](templates/repo-declarations.md). Record the
tracker choice, canonical **scope and stories** home (`issue-tracker` or
`project-docs`), workflow-state owner (`tracker` or `project-docs`) and pointer,
docs-layout homes, merge policy/authority and work-item runbook pointer. Use
pointers only; never duplicate runbook or policy content.
With `issue-tracker`, the parent issue is the canonical change scope/story home;
do not duplicate it. This block, not the GitHub scaffolding, is the completion gate.
With tracker `none`, use `project-docs` for both scope/story and workflow state.

## Docs layout

Ask where specs, runbooks and ADRs live. Defaults: reference
`engineering-docs`'s **Default homes** table by skill name; do not invoke it.
`setup-repo` owns the declaration.

## GitHub branch: steps 1-7

Only when the tracker is GitHub issues and the remote host is confirmed:

1. **Labels:** copy [`templates/labels.yml`](templates/labels.yml) and
   [`templates/workflows-labels.yml`](templates/workflows-labels.yml) to
   `.github/labels.yml` and `.github/workflows/labels.yml`; warn that
   `skip-delete: false` prunes undeclared labels.
2. **Ruleset:** add the [`ci-status` aggregate](templates/ci-status-aggregate.yml),
   then create the ruleset per
   [ruleset guidance](references/ruleset-and-ci.md), confirming its app id.
3. **Issue forms/security:** copy [`templates/SECURITY.md`](templates/SECURITY.md)
   to the root, or verify existing policy has a valid private route. Copy
   `templates/ISSUE_TEMPLATE/*.yml` to `.github/ISSUE_TEMPLATE/`; adapt gate
   wording and replace owner/repo. Confirm private vulnerability reporting is
   enabled, or replace `<private-reporting-route>` with a working confidential
   contact method. Publish only when the placeholder is gone and both routes
   work.
4. **PR template:** copy
   [`templates/pull_request_template.md`](templates/pull_request_template.md)
   into `.github/` and adapt evidence rows; retain decision and review sections.
5. **Work-item runbook:** copy [the doctrine](references/doctrine.md) into the
   target, adding its mechanics; the project decides merge authority.
6. **Project board:** create the [six-status board](references/project-board.md).
7. **CODEOWNERS/Dependabot:** adapt to the stack; the
   [`Dependabot` template](templates/dependabot.yml) keeps its dependency label.

## Stop conditions

Stop for an unnamed target, unconfirmed write permission, an unconfirmed
remote host before any `gh` command, replacement of an existing ruleset or
labels file, semantic conflict, or ambiguous `ci-status` dependencies. Ask
which jobs it should `needs:`.
