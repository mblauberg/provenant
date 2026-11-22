## Fixes

- Writer launches no longer create a private lane Cargo cache under
  `Library/Caches/provenant-lane-cargo`, write its ownership marker and
  `CACHEDIR.TAG`, or export `CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR`
  into it. Lanes do not build; writer Cargo uses its worktree default, and
  `PROVENANT_LANE_CARGO_LINT_TARGET` is gone from the lane environment. `gc`
  still retires existing leftovers: a cache holding only its ownership marker
  and directory tag is reclaimed in one pass whatever its owning project, while
  a cache with build output in another project is still kept as foreign.
  (#3802)
