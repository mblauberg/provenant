## Breaking changes

### Lanes and settlement

- JSON output renames, applied to stored run records by the run store migration (schema 15) and to graph checkpoints by the checkpoint migration (schema 3): `terminal_cause` is now `cause` (and `terminal_cause_code` is `cause_code`), `partial_signals` is `signals`, `terminal_evidence` is `terminal_basis`, `settlement_reason` is `settlement_class`, and graph `advance_error_code` and `advance_error` are `advance_code` and `advance_note`. The cause value `handoff_refused` is now `launch_handoff_refused`, and the advance code `writer_declared_stop` is `declared_stop`. Scripts that read the old keys must switch to the new ones. (#4020)
- The document explain `settlement` block has a new shape: `precedence` lists the layers, `knobs` is one entry per `lanes.settlement.*` key with its `key`, effective `value`, `layer`, `origin` and `allowed` values, and `warnings` holds the resolution warnings. Nothing reads the earlier shape, so scripts that parsed it must switch. (#4020)
