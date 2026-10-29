## Fixes
### Git and safety

- Writer lanes with publishing enabled can stage and commit again, and `git stash` works in them, including when the branch was packed-only (#2108, #2143, #2155).
- Publishing credentials stay available to the lane Git wrapper without changing shared Git state (#2146).
