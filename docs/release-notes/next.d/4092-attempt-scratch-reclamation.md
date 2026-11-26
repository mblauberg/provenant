## Fixes
### Disk and retention
- Settled attempt scratch whose run record is unreadable or cannot prove custody is reclaimed after the configurable `gc.scratch_grace` (default `1d`, source shown by `provenant gc`) instead of being kept forever. The grace is measured from the scratch tree's newest write, and fresh publication, writer-lock and continuation checks still gate removal. Settlement applies the same release path. (#4092)
- Read-only GC previews observe `gc.preview_budget` (default `1s`) instead of a hidden hard-coded second. When a preview or an applied sweep stops at its bound, exit 3 now names which budget stopped the pass and how to raise it. (#4092)
- Attempt scratch collection ranks candidates largest first within each resume page and reports reclaimed bytes per retention class. (#4092)
- `check` warns (`ATTEMPT_SCRATCH_HIGH`) when attempt scratch exceeds `gc.scratch_warning_share` (default `0.25`) of measured free space; zero disables the warning and small positive shares retain their precision. (#4092)
