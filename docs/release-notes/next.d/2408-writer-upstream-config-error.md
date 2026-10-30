## Fixes

- Writer lanes are no longer rejected with "utility operation has an invalid or unsupported value" when several writers launch at once. Their worktrees share the repository's `.git/config`, so concurrent `git config` writes were failing to lock the file and the real stderr was being discarded. The upstream write now reports the actual git failure (`git config` exit status and bounded stderr) and briefly retries only config lock contention, honouring the lane budget and cancellation ([#2408](https://github.com/mblauberg/provenant/issues/2408)).
