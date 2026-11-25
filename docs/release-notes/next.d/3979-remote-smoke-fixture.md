## Fixes

### Lanes and settlement

- `provenant upgrade --verify --host <peer>` and `provenant run smoke --host <peer>` now stage the read node's fixture into the peer's own provider scratch and name its path in the brief, instead of relying on a caller-local fixture path the peer cannot see. A graph node may declare a small project-relative fixture; the host carries its bytes with the submission, so no sender path crosses the boundary. A remote smoke writer now runs through a peer-owned worktree: `--branch` (with optional `--base`) is carried as provisioning intent and the peer creates the worktree in its own clone, rather than skipping the write node with "remote placement does not accept caller paths". (#3979)
