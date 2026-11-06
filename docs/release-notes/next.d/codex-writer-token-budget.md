## Fixes
### Lanes and settlement

- The writer token budget does not stop Codex writers: `codex exec` reports usage only when its one turn has finished. A budget stop leaves a cherry-pick, revert or rebase in progress uncommitted, with a settlement note, as it already did for a merge. Skipped and failed commits remain reported alongside the budget outcome (#2992).
