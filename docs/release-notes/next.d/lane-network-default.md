## Behaviour changes

- Every lane now defaults to open network access across all adapters and modes,
  allowing read-only lanes to run tools such as `gh` or fetch documentation
  without requiring `--network on`. Passing `--network off` still restricts a
  lane to its provider endpoints, or to local network access when the Postgres
  capability is requested.
