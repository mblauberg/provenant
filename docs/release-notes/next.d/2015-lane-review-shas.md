## Additions

- Pass a chair-verified full commit SHA with `--head` and optionally a full SHA with `--base`. The host records the merge base with `origin/HEAD` when the base is omitted, injects the range into the brief and exports `PROVENANT_LANE_HEAD` and `PROVENANT_LANE_BASE`. Peer writer lanes now receive the base when a head is supplied.
