## Breaking changes
### Pipelines and graphs

- **Replace `review-shape-f?-v?-s?` composites and `review-topology`** with `review-shape` and the `review-frame`, `review-verify` and `review-synthesis` launch values. Graph nodes can use `when = { name = value }` to skip a node when a condition does not hold. (#3172)
