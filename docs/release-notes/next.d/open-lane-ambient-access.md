## Behaviour changes
### Git and safety

- Lanes now have ambient access to ordinary files, tools, caches, terminals,
  sockets, databases and open networks. Credentials and preservation archives,
  Provenant host state, shared Git metadata, read-only sources and other owned worktrees
  stay denied, apart from selected authentication, open-network GitHub CLI reads
  and narrowly scoped attempt inputs and private state; host restrictions still
  apply.
