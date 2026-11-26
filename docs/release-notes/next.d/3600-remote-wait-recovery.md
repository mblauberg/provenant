## Fixes

### Lanes and settlement

- A remote lane that finishes while its home endpoint is unreachable no longer holds a wait hostage to the first error. A graph wait treats an unavailable remote reading as pending and keeps rereading within its budget; the next successful terminal reading re-drives settlement through the journal-locked advancement owner, and a wait that ends still pending names its last failed reading instead of the first. A successful advance clears the transport error held from the earlier failed attempt, so `status` shows the settled state. (#3600)
