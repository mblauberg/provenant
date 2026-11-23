## Fixes
### Fabric

- Batch documents with local integration admit host-planned slice worktrees
  without pre-creating them, then provision both checkouts; the integrate-target
  check skips a writer that has no branch until provisioning. `worktree_missing`
  now names the path and node in its message, diagnostic and JSON error. A
  provider-relative model such as `opencode-go/<model>` is canonicalised with its
  adapter instead of producing an unnamed `invalid_input`, and every invalid
  launch input names the field and reason it rejected. Relative writer paths
  resolve from the project's primary root from every checkout, and
  `run --explain` runs the same admission checks as a launch. (#3821)
