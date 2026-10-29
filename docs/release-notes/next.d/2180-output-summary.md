## Additions

- `output` shows the whole final result up to 12 KiB, with a settlement line and changed-file evidence. Larger results keep their head and tail and name the omitted bytes and `--full`. It works locally and with `--host`, including `--json`.
- `output --tail N` now reads peer artifacts as well as local artifacts. It retains at most 500 lines within `--max-bytes` and refuses changing artifacts during a peer scan.
