# Main-branch ruleset and the ci-status aggregate

Use this pattern with the target repository's ruleset and CI workflow.

## Why one aggregate check

A branch ruleset should pin exactly one required status check, not one per
build job. Pinning many individual jobs means every new job needs a manual
ruleset edit, and a renamed job silently stops being enforced. Instead, add a
single `ci-status` job (template:
[`../templates/ci-status-aggregate.yml`](../templates/ci-status-aggregate.yml))
that `needs:` every real job, runs `if: always()`, and fails unless each
needed job's result is `success`, or `skipped` for a job the change filter
explicitly left unselected. Any other skip fails the check. That one job name is what the
ruleset pins.

## Creating the ruleset

1. Inspect the target's check runs with `gh api` to identify the intended CI app.
2. Decide the review and base-update requirements and record them in the
   repository's runbook. A strict base-update requirement reruns checks when the
   base moves; without it, the check covers the pull request head.
3. Create the branch ruleset through the [GitHub rulesets API](https://docs.github.com/en/rest/repos/rules)
   with the aggregate check bound to that app. Verify the result below.

## Verifying it landed

```sh
gh api repos/<owner>/<repo>/rulesets
gh api repos/<owner>/<repo>/rulesets/<id>
```

The second call's `rules[].parameters.required_status_checks` must show the
`ci-status` context bound to the intended `integration_id`.
