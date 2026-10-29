## Fixes

- Writer lanes with publishing enabled can stage and commit again, and `git stash` works in them. The host creates a loose branch ref and configures its upstream before launch, so a writer can commit when its branch was packed-only (#2108, #2143, #2155).
- Publishing credentials stay available to the lane Git wrapper without changing shared Git state (#2146).
