## Behaviour changes

### Git and safety

- `land` prints the candidate ref, checks URL and state while it waits on a gate, then elapsed time every `land.progress_seconds` (default 60). A candidate whose tree already passed the required checks on a member head skips the gate and records the reused head in the receipt; set `land.reuse_tested_tree = false` to always gate. Each land removes pending records and `land/*` refs of merged or closed pull requests. (#3376)
