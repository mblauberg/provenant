## Fixes

### Lanes and settlement

- Every lane with open network, read-only lanes included, now gets GitHub access by default, so lanes can read issues and comment, and writers `git push` to their own branch. Set `lanes.publish = false` to withhold it. `/bin/ps` stays unavailable in a lane because macOS refuses setuid programs under the sandbox; the configuration runbook now says so.
- The host GitHub login lookup retries a timed-out or killed `gh auth token`, reuses the token across fallback routes, and prints a launch warning naming the cause when it fails. Previously a lookup stalled by heavy host load silently left the lane without a token, so `git push` failed with `could not read Username`.
