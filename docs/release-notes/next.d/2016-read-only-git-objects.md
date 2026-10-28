## Fixes

- Allow read-only lanes to run Git commands that need temporary objects in
  private scratch through the shell Git wrapper, while keeping the shared
  object store read-only. Scope the override to the bound repository and its
  linked worktrees so other repositories and submodules retain their own stores.
