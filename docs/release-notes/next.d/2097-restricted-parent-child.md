## Fixes
### Lanes and settlement

- A parent with restricted network access can launch children, which keep its restriction. A child continuation is limited to the requesting parent's network, or to the provider-only profile when the parent's context is unavailable; on macOS that profile also permits outbound TCP 443 beyond provider endpoints, tracked in #2118 (#2097).
