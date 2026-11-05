## Breaking changes
### Pipelines and graphs

- **The `review-council` preset is now `review`.** Finish graphs with an unresolved deferred review before upgrading, then use `review` for new runs (#2870).
- Review verdicts must use the final-line wire grammar; first-line legacy verdicts followed by prose are no longer accepted (#2870).
