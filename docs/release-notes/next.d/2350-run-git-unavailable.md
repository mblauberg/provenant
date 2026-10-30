## Fixes
### Lanes and settlement

- Run operations that return `RunError` now report a missing Git executable as "Git executable unavailable" with the `GIT` code. Launch handoff reports the reason as a handoff refusal (#2350).
