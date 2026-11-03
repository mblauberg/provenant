## Fixes
### Lanes and settlement

- Writer settlement completes when its branch has advanced beyond the lane head. Reconcile reports publication errors and offers `--abandon RUN/ATTEMPT --reason TEXT` to release an unsettleable fence while keeping its evidence (#2782).
