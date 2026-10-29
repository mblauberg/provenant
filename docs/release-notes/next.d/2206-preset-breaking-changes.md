## Breaking changes
### Pipelines and graphs

- **`review-council` and `big-batch` take lists.** Pass a `lenses` list to `review-council` instead of `correctness_tier`, `simplicity_tier` and `risk_tier`, and a `lanes` list to `big-batch` instead of `lane_one_*` and `lane_two_*` (#2206).
- **The role fields `route` and `required_verdict` are gone.** Use `tier` or `routes` (#2206).
- **Graph and preset `read_roots` must lie inside the project.** List any other directory in the host allow-list `[domains.fabric.graph] allowed_read_roots` (#2206).
