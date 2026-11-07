## Behaviour changes
### Git and safety

- Lanes now have ambient access to ordinary files, tools, caches, terminals,
  sockets, databases and open networks on macOS. Credentials and preservation
  archives, Provenant host state, shared Git metadata, read-only sources and
  other owned worktrees stay denied, apart from selected authentication,
  open-network GitHub CLI reads and narrowly scoped attempt inputs and private
  state. The host can further restrict reads, writes and network through
  `[domains.run.host_policy]`; Linux refuses launch because it has no enforcing
  backend. Cursor, OpenCode and read-only Claude attempts created under the old
  layout cannot resume, and no compatibility shim is provided.
