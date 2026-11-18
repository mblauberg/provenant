## Pull request landing

- `pr sync` merges a pull request's base into its branch and records a carry receipt. Landing can reuse independent approval across a clean base merge or a formatter-only change when the configured formatter reproduces both trees. Conflict resolutions that alter the merge tree still require fresh approval. (#3495, #3501)
