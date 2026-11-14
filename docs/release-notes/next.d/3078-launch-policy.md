## Behaviour changes
### Lanes and settlement

- Launch policy now reports each rule's strength, source and override in `provenant config show` and the lane preamble. Resource thresholds and writer census warn by default; use `policy.CODE=refuse` for strict enforcement. Writer scope remains writable across the worktree, with out-of-scope paths reported at settlement. (#3078, #3208, #3275)
