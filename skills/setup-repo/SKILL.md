---
name: setup-repo
description: "Use to set up a repository's tracker, issue forms, labels, ruleset and runbook."
---

# Setup

Scaffold the process a repository needs so agents can run it with the human
near or out of the loop. Setup is the one place that sets how much autonomy a
repository grants.

## Classify

"This repo" in a checkout means that checkout; otherwise name the target. Read
its `Repository process` declaration, `MAINTAINING.md`, `.github/**`,
`.provenant/**` and runbook first, and ask only what it has not settled. Show
one diff naming the target and get one approval. Mark each action **keep**
(exact match), **create** (absent), **adapt** (compatible, propose the merge)
or **conflict** (semantic mismatch: ask). Never overwrite; a re-run on a set-up
repository produces no diff. Confirm the remote host before any `gh` command.

## Declare the process

Amend the `Repository process` block in `MAINTAINING.md` from
[`templates/repo-declarations.md`](templates/repo-declarations.md): tracker,
scope home (`issue-tracker` or `project-docs`), workflow-state owner, docs
layout, merge policy and runbook pointers. Pointers only; never copy policy.
With tracker `none`, both homes are `project-docs` and the GitHub steps are
skipped. Specs, runbooks and ADRs take `engineering-docs`' default homes unless
the owner says otherwise.

Record the chosen autonomy and landing gates in the runbook. Project templates
go under `.provenant/templates/`; a template grants no new authority. Run
`provenant config check`.

## GitHub steps

1. **Labels:** copy [`labels.yml`](templates/labels.yml) and
   [`workflows-labels.yml`](templates/workflows-labels.yml). The sync prunes
   undeclared labels, so warn before the first run.
2. **Issue forms:** copy [`ISSUE_TEMPLATE/`](templates/ISSUE_TEMPLATE/config.yml)
   (Epic, Feature, Task, Bug). Confirm private vulnerability reporting works,
   then copy [`SECURITY.md`](templates/SECURITY.md).
3. **Ruleset:** add the [`ci-status` aggregate](templates/ci-status-aggregate.yml)
   and a ruleset requiring that one exact-head check, per
   [ruleset guidance](references/ruleset-and-ci.md). Ask which jobs it
   `needs:`.
4. **PR template:** copy
   [`pull_request_template.md`](templates/pull_request_template.md) and adapt
   the evidence rows.
5. **Runbook:** seed the work-item runbook from
   [the runbook seed](references/runbook-seed.md).
6. **Dependabot and CODEOWNERS:** adapt [`dependabot.yml`](templates/dependabot.yml).

Stop and ask before replacing an existing ruleset or labels file, on a
semantic conflict, or when write permission or the host is unconfirmed.
