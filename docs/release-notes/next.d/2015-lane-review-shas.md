## Additions
### Lanes and settlement

- `--head` takes a chair-verified full commit SHA and `--base` optionally a full SHA, defaulting to the merge base with `origin/HEAD`. The range goes into the brief and is exported as `PROVENANT_LANE_HEAD` and `PROVENANT_LANE_BASE`, and peer writer lanes now receive the base when a head is given (#2015).
