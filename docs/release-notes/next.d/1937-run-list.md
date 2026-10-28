## Fixes

- Fabric run listings now return compact attempt summaries, newest queued first.
  Follow `next_after` to page towards older attempts and use `op: status` for
  the full attempt record, including usage and execution evidence.
