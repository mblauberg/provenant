## Fixes

- Allow restricted-network parents to launch children while preserving their network restriction and stable child identity.
- Preserve child-store continuations while bounding their network by the requesting parent, or the same provider-only profile as other provider-only lanes if its sealed context is unavailable. On macOS, that profile currently permits outbound TCP 443 beyond provider endpoints ([#2118](https://github.com/mblauberg/provenant/issues/2118)).
