## Additions

- List local projects and registered chair sessions with `console projects` and `console sessions`. Sessions without a summary report it as unavailable.
- Session discovery examines up to 16,384 entries across projects and sets `scan_truncated` when the limit is reached. Results over 1 MiB are refused without writing a file.
