## Fixes
### Chair, inbox and console

- A session checkpoint now warns only above 8 KiB, and the warning names the project's `checkpoint_soft_bytes` budget. A project with a raised budget stays quiet until its own budget is exceeded. Before, the setting had no effect and a project budget larger than 8 KiB still warned while claiming the shared 2 KiB default (#2350).
- The session-start and compaction state block omits lane sections that have no lanes, and prints nothing when there is no lane or pull request state to show. Before, it listed `Lanes in flight (0)` even when nothing was in flight (#2350).
- The save command that hooks inject names the checkout the hook ran in with `--project`. A save from a scratch directory outside that checkout now reaches the same project, where it failed before (#2350).
