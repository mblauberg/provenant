## Fixes
### Chair, inbox and console

- A session checkpoint now warns only above 8 KiB. Before, every save over 2 KiB warned. The warning still states the 2 KiB soft budget as guidance (#2350).
- The session-start and compaction state block omits lane sections that have no lanes, and prints nothing when there is no lane or pull request state to show. Before, it listed `Lanes in flight (0)` even when nothing was in flight (#2350).
- The save command that hooks inject names the checkout the hook ran in with `--project`. A save from a scratch directory outside that checkout now reaches the same project, where it failed before (#2350).
