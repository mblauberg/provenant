## Fixes

- A parent with restricted network access can launch children, which keep its restriction. A continuation of a child store is bounded by the requesting parent's network, or by the provider-only profile when the parent's sealed context is unavailable. On macOS that profile currently also permits outbound TCP 443 beyond provider endpoints ([#2118](https://github.com/mblauberg/provenant/issues/2118)) (#2097).
