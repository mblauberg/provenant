## Fixes
### Lanes and settlement

- Cancelling a lane that has not launched now settles it as cancelled and frees its queue place and writer before `stop` returns, a launcher waiting on setup stops at once, and a `starting` attempt whose launcher died settles by itself.
