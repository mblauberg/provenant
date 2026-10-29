## Fixes
### Lanes and settlement

- A missing Git executable is now reported as one error, "Git executable unavailable", with the `GIT` code everywhere in the run crate. Writer preparation and launch previously reported it as `INVALID` with differing messages (#2350).
