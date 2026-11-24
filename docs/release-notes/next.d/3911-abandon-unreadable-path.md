## Fixes

### Lanes and settlement

- `provenant reconcile --abandon` no longer refuses a removed writer checkout because a system process holds a file in a directory this user cannot read. Such a path is ruled out when its nearest readable directory is none of the checkout's existing ancestors. (#3911)
