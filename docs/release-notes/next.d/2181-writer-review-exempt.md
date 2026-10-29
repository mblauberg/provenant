## Behaviour changes
### Lanes and settlement

- Writers no longer stop at `review_required` or `uncommitted` over paths every implementation changes. The new `[domains.run] review_exempt` list defaults to release-note fragments, `docs/specs/`, `Cargo.lock` and `.tracker-input`, and Git metadata, hooks, provider control roots and CI workflows can never be listed (#2181).
