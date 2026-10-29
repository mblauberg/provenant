## Behaviour changes

- An uncatalogued model, or an effort the catalogue does not record for that model, is refused at submit with the nearest catalogued IDs or the supported efforts, before any run record exists (#2350).
- A `--worktree` that is detached or whose writer is busy is refused at submit and names the HEAD or the holder attempt. A replayed operation ID still returns its original run (#2350).
- A launch failure says which step failed, such as "launch preparation failed: publication reservation: ...", instead of the bare cause (#2350).
