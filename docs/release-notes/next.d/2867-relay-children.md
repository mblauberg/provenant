## Fixes
### Lanes and settlement

- Wake graph execution after live child admission, load user and repository
  templates for child requests, and cancel failed parents' attached children.
  Detached children no longer hold up the parent's dependants (#2867).
