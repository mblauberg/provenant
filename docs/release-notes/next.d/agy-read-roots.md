## Fixes

- Read-only Agy lanes can now read a `--read-root` outside their working
  directory. Agy's headless mode auto-denied those reads and the lane failed
  with "provider result is empty or incomplete" (#1729).
