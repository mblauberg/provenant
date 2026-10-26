## Behaviour changes

- Every peer request now carries a `trace` (sending host, actor and the
  operation the action names). Peer ingress refuses a request whose
  `trace.host` is not its `source_host`. A usage event under
  `usage.project_attribution` now records the project as `trace.project`
  instead of `project_id`. Earlier lines in `usage.jsonl` keep counting in the
  aggregate rows but no longer in the per-project rows (#1571).
