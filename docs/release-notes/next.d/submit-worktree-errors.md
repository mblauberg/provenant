## Fixes

- Fabric writer submissions now report `worktree_unregistered` with the worktree
  path when `git worktree add` created the tree without Provenant's custody
  registration. An advisory claim does not register a writer. Use
  `provenant worktree create <new-branch> --base <branch>` to create a
  registered writer at the existing branch's tip.
  Submit I/O errors retain the filesystem operation and path; failures without
  a more specific path identify the bound state root (#1777).
- The changed peer error payload uses Fabric protocol 4. Upgrade peers
  together; protocol 3 peers are refused before action decoding.
