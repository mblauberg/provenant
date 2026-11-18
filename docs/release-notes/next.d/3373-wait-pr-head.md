## Fixes

### Waits and CI observations

- `wait --pr N --head SHA` pins the wait to one head: until GitHub reports that
  exact SHA the reading stays pending, so a failed check on the previous head
  can no longer produce a verdict seconds after a push. The wait always names
  the head being watched. Without `--head`, when the PR's branch is known
  locally and its tip differs from GitHub's head, it warns and waits for the
  local tip instead. (#3373)
