## Fixes
### Lanes and settlement

- Writer cache preparation no longer holds up Git setup for other lanes in the same repository. A refused peer branch collision reports that the existing branch was kept. `provenant run --wait` returns while a launch is still preparing: the lane reports `waiting` with its phase, and preparation carries on. Local preparation has at least five seconds and a larger window for multi-task submissions (#2934).
