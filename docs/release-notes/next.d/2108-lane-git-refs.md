## Fixes

- The host materialises a loose lane branch ref and configures its upstream before launch, so a writer can commit when its branch was packed-only. A lane may update only its own `refs/remotes/origin/<lane-branch>` tracking ref and reflog, with their locks.
