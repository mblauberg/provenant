## Additions

- `output --summary` reports settlement, available changed-file evidence and the final result paragraph within a bounded summary. It works locally and with `--host`, including `--json`.
- `output --tail N` now reads peer artifacts as well as local artifacts. It retains at most 500 lines within `--max-bytes` and refuses changing artifacts during a peer scan.
