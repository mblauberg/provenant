## Behaviour changes

- Writers no longer stop at `review_required` or `uncommitted` over paths every implementation changes. The new `[domains.run] review_exempt` list (default: release-note fragments, `docs/specs/`, `Cargo.lock` and `.tracker-input`; replaceable per project under `[domains.run.projects."ID"]`) exempts committed changes, renamed or unmerged entries and uncommitted leftovers on those paths. Git metadata, hooks, provider control roots and CI workflows can never be listed. `provenant status` ends with the list in force (#2181).
