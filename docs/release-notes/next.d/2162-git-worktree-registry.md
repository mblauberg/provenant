## Breaking changes
### Git and safety

- `worktree adopt` is removed. Use Git-created linked checkouts directly; Git's inventory now establishes membership, including for checkouts recreated at a previously used path. Lock contention reports busy, and settlement retains writer exclusion through import and publication (#2162).
