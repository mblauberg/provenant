## Additions

### Lanes and settlement

- Finished lanes clean up after themselves. Once a lane's work has landed, settlement or the next `gc` pass removes its worktree, local branch, remote branch and remote-tracking ref, but only refs Provenant created, each deletion leased on the tip it checked. A created ref that has not landed stays for `cleanup.unlanded_days` (default 14). Every artefact reports one JSON outcome, and a failed removal warns without failing settlement (#3039).
