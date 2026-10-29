## Additions

- `--head` takes a chair-verified full commit SHA and `--base` optionally a full SHA. When the base is omitted, the host uses the merge base with `origin/HEAD`. The range goes into the brief and is exported as `PROVENANT_LANE_HEAD` and `PROVENANT_LANE_BASE`. Peer writer lanes now receive the base when a head is given (#2015).
