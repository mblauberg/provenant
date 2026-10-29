## Fixes

- Lane launches no longer refuse over hard-linked build or dependency files (`target/debug/incremental`, `node_modules/.pnpm`), over a compiled profile above 64 KiB, or over a path anchor for a file whose directory does not exist yet. The hard-link census, its per-path write denials and its caps are gone: `deny file-link` still stops lanes creating links, and launch checks only that the host Git `config`, `index`, `hooks/*` and `info/*` have one name. Profiles may reach 512 KiB and the error names the size; held-path failures name the path and the check (#2164, #1951).
