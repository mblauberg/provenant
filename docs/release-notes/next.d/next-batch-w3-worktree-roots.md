## Added

### Worktrees

- Worktree layout roots may lie outside the checkout: the layout port accepts
  absolute and parent-relative roots from the repository or host port table,
  `config ports` reports each root with its source, and creation, listing,
  custody and GC treat a declared root like the default one. (#3285)
