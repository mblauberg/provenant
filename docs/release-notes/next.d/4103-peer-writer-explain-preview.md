## Fixes
### CLI and Fabric
- `run --explain` no longer admits a peer-placed writer against the caller checkout. The writer is reported as proposed on the peer, naming the peer and the worktree it will provision from its branch; the explanation is no longer refused for the caller's state root. (#4103)
- `upgrade --verify --dry-run` now builds its `run smoke --explain` preview with the same writer branch and base as the real verify. The preview no longer reports a missing base or an admission refusal the launch would not produce. (#4103)
