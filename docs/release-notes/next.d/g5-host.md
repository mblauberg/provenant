## Fixes
### Pipelines and graphs

- Graph `run` retains peer placement with `--host` and attempt settings, plus
  `--model`, `--effort`, `--account` and advanced `--operation-id` controls.
  Set `attempt.build_heavy`, `attempt.confidential` and `attempt.provider_timeout`
  with `--set`; start delivery with `--use deliver`.
