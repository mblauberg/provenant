## Fixes
### Run store

- Read-only views of a run (status, lane listings and output paging) no longer force an `F_FULLFSYNC` on every sealed read, so `provenant status` stops spending about 35 seconds in `fcntl` on macOS. Mutating and recovery reads still sync ([#2409](https://github.com/mblauberg/provenant/issues/2409)).
