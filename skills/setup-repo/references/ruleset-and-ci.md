# Main-branch ruleset and the ci-status aggregate

Pattern from provenant's live ruleset and its `ci-status` job in `.github/workflows/ci.yml`.

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

```sh
gh api repos/<owner>/<repo>/rulesets --method POST --input - <<'JSON'
{
  "name": "Protect main",
  "target": "branch",
  "enforcement": "active",
  "conditions": {
    "ref_name": { "include": ["~DEFAULT_BRANCH"], "exclude": [] }
  },
  "rules": [
    {
      "type": "pull_request",
      "parameters": {
        "required_approving_review_count": 0,
        "dismiss_stale_reviews_on_push": false,
        "required_reviewers": [],
        "require_code_owner_review": false,
        "require_last_push_approval": false,
        "required_review_thread_resolution": false,
        "allowed_merge_methods": ["merge", "squash", "rebase"]
      }
    },
    {
      "type": "required_status_checks",
      "parameters": {
        "strict_required_status_checks_policy": false,
        "do_not_enforce_on_create": false,
        "required_status_checks": [
          { "context": "ci-status", "integration_id": 15368 }
        ]
      }
    }
  ]
}
JSON
```

`integration_id: 15368` is the GitHub Actions app; pinning it (not just the
`context` string) stops a same-named check from a different app or a
hand-posted commit status from satisfying the rule. Confirm the id for the
target repo/org before reuse:

```sh
gh api repos/<owner>/<repo>/commits/main/check-runs \
  --jq '.check_runs[] | {name, app: .app.name, integration_id: .app.id}'
```

`required_approving_review_count: 0` is provenant's own choice for a
single-maintainer repo; a team repo should raise it and decide
`require_code_owner_review` deliberately rather than copy the zero.

`strict_required_status_checks_policy` is also a choice. `false` (provenant's
live setting, and the template above) lets a head merge while it is behind
`main`, so landing needs no update-merge and `ci-status` gates the exact head
only. `true` requires an up-to-date head at merge time: every merge to `main`
then forces each open pull request to update and rerun its exact-head checks.
Pick `true` when a green check must always describe the post-merge base, and
record the choice in the repository's runbook.

## Verifying it landed

```sh
gh api repos/<owner>/<repo>/rulesets
gh api repos/<owner>/<repo>/rulesets/<id>
```

The second call's `rules[].parameters.required_status_checks` must show the
`ci-status` context bound to the intended `integration_id`.
