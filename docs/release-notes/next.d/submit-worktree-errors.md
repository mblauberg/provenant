## Fixes

- Fabric writer submissions now report `worktree_unregistered` with the worktree
  path when `git worktree add` created the tree without Provenant's custody
  registration. An advisory claim does not register a writer. Use
  `provenant worktree create <new-branch> --base <branch>` to create a
  registered writer at the existing branch's tip.
  Submit I/O errors retain their operation and relevant absolute path when
  known. Writer errors now name Git runtime and executor setup, utility budget
  setup, project discovery, repository opening, HEAD reading and decoding,
  branch parsing and resolution, and base decoding and parsing. Git utility
  timeouts (5 s) retain the name of the operation that timed out. Writer
  reservation, snapshot, quarantine and terminal check failures also have
  operation names. The peer's exact bare `io` failure was not reproduced;
  filesystem fault injections demonstrate the diagnostic loss (#1777).
- The changed peer error payload uses Fabric protocol 5. Upgrade peers
  together; protocol 4 peers are refused before action decoding.
