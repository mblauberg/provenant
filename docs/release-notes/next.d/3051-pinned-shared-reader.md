## Fixes
### Pipelines and graphs

- Read-only lanes using `light` now read the bound writer checkout. A dirty checkout or a HEAD that differs from the pinned commit uses a snapshot, so the review describes its recorded commit (#3051).
