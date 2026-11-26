## Fixes

### Lanes and settlement

- A guardian now records a durable attempt tag at provider spawn: the boot, the spawned process group and session, and whether an OS containment mark is in effect. It also passes the attempt id as an internal mark in the provider's environment, inherited across fork, exec and setsid. When the guardian dies before publishing `process-exit.json`, reconcile uses that tag to name a detached unsandboxed process in status as an escaped PID (and in a settled exit's `unresolved_pids`) instead of leaving it invisible; an `ORPHAN_PROCESS_ATTRIBUTED` warning on the attempt keeps it visible in `status` while the fence holds. Group/session and inherited-mark attribution grant no signal authority, so such a process is reported and never signalled; lanes with an OS mark still reap escapees as before. Hosts without containment warn once per change with the setting source (`policy.LAUNCH_PROCESS_ATTRIBUTION_NONE`, default `warn`, set `off` to suppress). (#4006)
