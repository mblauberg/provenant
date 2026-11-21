## Fixes

### Fabric

- Cross-host launches now fetch the pinned base from the peer clone's origin
  before resolving it, so a stale peer clone no longer refuses a launch whose
  base commit exists upstream. If the commit is still missing after the fetch,
  the refusal names the clone and the fetch outcome and is reported as a
  host-state failure rather than invalid input. If the launch is cancelled
  during the fetch, the cancellation is preserved instead of being reported as
  a failed fetch. (#3742)
