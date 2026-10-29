## Behaviour changes

- Writer lanes now have a complete private Git directory, including their index,
  refs and operation state, while shared Git metadata stays read-only.
- Import uses Git receive quarantine and strict pack checks. Symlinks and foreign
  alternates are refused, transferred bytes and runtime scale with private store
  size, and abandoned import directories are reclaimed after 24 hours.
- Launch bases stay pinned until terminal publication; terminal outcomes remove
  temporary refs, and restored index flags cannot hide uncommitted changes.
- Drain writers before upgrading. Interrupted older attempts with private objects
  can resume; attempts without them name host recovery and a fresh run.
