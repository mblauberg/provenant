## Fixes
### Lanes and settlement

- Codex writer lanes now stop at their token budget when a completed turn takes them past it, and the host commits their work. Codex reports usage only at `turn.completed`, so a single long turn is not stopped mid-turn. A budget stop leaves a cherry-pick, revert or rebase in progress uncommitted, with a settlement note, as it already did for a merge (#2992).
