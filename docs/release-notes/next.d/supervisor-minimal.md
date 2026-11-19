## Breaking changes

### Lanes and settlement

- The lane supervisor keeps only what providers cannot do: start, exit
  watching, process-tree cleanup, durable records and the writer, credential
  and authority boundaries. It no longer samples CPU or disk per lane, tracks
  output silence, spares processes by command name or re-checks the provider
  executable before spawn, and it scans cleanup holders once per guardian
  rather than every 30 s. Status no longer reports provider silence. (#3566)
- Process-tree sampling runs only when needed: an admission class with
  `memory_per_slot` turns it on every 5000 ms, and
  `domains.run.resource_sample_interval_ms` (1..=60000) sets it explicitly;
  `provenant config show --full` names the source. Otherwise it is off and
  `PROVENANT_PROCESS_TREE` is unset. A configured interval now takes effect:
  guardians previously could not read config and always sampled every 5000 ms.
  (#3566)
- Retained custody no longer polls the host every 20 ms while it stops a lane's
  survivors: each stop takes at most three censuses and waits on exit watches
  between signals. (#3566)
- `lanes.silence_seconds`, `lanes.supervision` and the `GUARDIAN_DISK` policy
  are removed; delete them from config. (#3566)
