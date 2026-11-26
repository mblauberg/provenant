## Fixes

### Lanes and settlement

- A remote lane that finishes while its home endpoint is unreachable no longer holds a wait hostage to the first error. A graph wait treats an unavailable remote reading as pending and keeps rereading within its budget; the next successful reading settles the lane and clears the earlier failure, and a wait that ends still pending names its last failed reading instead of the first. A successful reading of a finished lane also clears the transport error held from the earlier failed advance, so `status` shows the settled state. (#3600)
