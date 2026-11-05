## Breaking changes
### Pipelines and graphs

- **The `review-council` preset is now `review`.** Finish graphs with an unresolved deferred review before upgrading, then use `review` for new runs (#2870).
- Review verdicts must use the final-line wire grammar; first-line legacy verdicts followed by prose are no longer accepted (#2870).
- `provenant review --json` now reports `graph`, `graph_id`, `status_command`, `document` and `continue`. It also reports `reviewers` from the graph checkpoint, including pending lenses with no run ID yet. The former top-level `verdicts`, `shape`, `selection_source`, `lens_reasons`, `size`, `kind`, `risk`, `risk_tags` and `target` fields are replaced by graph and provenance data. Exit status follows the final panel verdict. Explain uses the preset graph envelope, and lowering errors use preset diagnostics.
