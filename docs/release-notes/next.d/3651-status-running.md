## Fixes

### Chair, inbox and console

- `provenant status` now counts and lists a lane with a live attempt as running instead of reporting the graph checkpoint's pending status; the compact summary, the listing rows and the `status NAME` detail derive their state from the same node and attempt facts, so a graph waiting for admission still reads as pending. (#3651)
