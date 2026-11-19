## Breaking changes

### Lanes and settlement

- The lane supervisor keeps only what providers cannot do: start, exit
  watching, process-tree cleanup, durable records and the writer, credential
  and authority boundaries. It no longer samples CPU or disk per lane, tracks
  output silence, spares processes by command name or re-checks the provider
  executable before spawn, and it scans cleanup holders once per guardian
  rather than every 30 s. Status no longer reports provider silence.
- Process-tree sampling is opt-in. To keep `PROVENANT_PROCESS_TREE` or
  admission memory limits, set `domains.run.resource_sample_interval_ms`
  (1..=60000); without a sample a memory limit is skipped with a warning.
- `lanes.silence_seconds`, `lanes.supervision` and the `GUARDIAN_DISK` policy
  are removed; delete them from config.
