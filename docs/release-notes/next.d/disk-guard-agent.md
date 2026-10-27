## Additions

- macOS default-state installs now include a per-user disk guard that checks
  space every two minutes. It warns and runs GC below 60 GiB, pauses matching
  build processes owned by the current user below 25 GiB and resumes them at
  50 GiB. Configure `[disk_guard]` in `config.toml` and rerun install to change
  its thresholds, process names or enable flag. Uninstall resumes saved paused
  processes before removing the owned agent (#1784).
