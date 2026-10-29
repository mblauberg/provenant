---
name: setup-repo
description: "Use when asked to set up or scaffold a repository's process: tracker, issue forms, labels, branch ruleset, preset rules, roles, autonomy level, docs layout or work-item runbook. Not for a repo's day-to-day GitHub mechanics; use its runbook."
---

# Setup

Scaffold the process a repository needs so agents can run it with the human near or
out of the loop. Setup is the one place that sets how much autonomy a repo grants.

## Gate and classify

Name the target; never infer it from the working directory. Read the target's
`Repository process` declaration, `MAINTAINING.md`, `.github/**`, `.provenant/**` and
its runbook before proposing anything. Ask only what the repo has not already settled.

Show one diff and get one approval. Mark each action **keep** (exact match),
**create** (absent), **adapt** (compatible, propose the merge) or **conflict**
(semantic mismatch: stop and ask). Never overwrite. A re-run on a set-up repo must
produce no diff. Confirm the remote host before any `gh` command.

## Declare the process

Amend the `Repository process` block in `MAINTAINING.md` from
[`templates/repo-declarations.md`](templates/repo-declarations.md): tracker, scope home
(`issue-tracker` or `project-docs`), workflow-state owner, docs layout, merge policy
and runbook pointers. Pointers only; never copy policy. With tracker `none`, both
homes are `project-docs` and the GitHub steps are skipped. Ask where specs, runbooks
and ADRs live, and apply engineering-docs' default homes.

## Autonomy

Record the chosen level in the runbook and wire it into config:

- `.provenant/preset-rules.toml` selects the preset: `by_readiness` (underspecified
  work goes to `scope-then-run`), `by_risk`, `by_paths` (for example a security path to
  `review-council`).
- `.provenant/roles.toml` holds project lenses and strictness, such as the verifier's
  `family = "cross_required"`.
- The `deliver` preset parameters `land_tier` and `decision_session` set what lands
  unattended. Routine and substantial work land on green CI by default, and crucial
  work asks the session. A repo file cannot lower `land_tier`; say so rather than try.

Confirm each with `provenant preset explain NAME` and `provenant config check`.

## GitHub steps

Only with tracker GitHub issues and a confirmed host:

1. **Labels:** copy [`labels.yml`](templates/labels.yml) and
   [`workflows-labels.yml`](templates/workflows-labels.yml). The sync prunes any
   undeclared label, so warn before the first run. Labels signal handling only, never
   type or status.
2. **Issue forms:** copy [`ISSUE_TEMPLATE/`](templates/ISSUE_TEMPLATE/config.yml): Epic,
   Feature, Task and Bug. The spec fields sit on Feature and Task. Confirm private
   vulnerability reporting works, then copy [`SECURITY.md`](templates/SECURITY.md).
3. **Ruleset:** add the [`ci-status` aggregate](templates/ci-status-aggregate.yml), then
   create a ruleset requiring that one exact-head check, per
   [ruleset guidance](references/ruleset-and-ci.md). Ask which jobs it should `needs:`.
4. **PR template:** copy [`pull_request_template.md`](templates/pull_request_template.md)
   and adapt the evidence rows.
5. **Runbook:** seed the declared work-item runbook from
   [the runbook seed](references/runbook-seed.md) and add the project's mechanics.
6. **Dependabot and CODEOWNERS:** adapt [`dependabot.yml`](templates/dependabot.yml).

## Stop

Stop for an unnamed target, unconfirmed write permission or host, replacement of an
existing ruleset or labels file, a semantic conflict, or ambiguous `ci-status`
dependencies.
