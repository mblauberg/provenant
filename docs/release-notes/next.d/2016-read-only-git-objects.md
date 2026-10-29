## Fixes
### Git and safety

- Read-only lanes can run Git commands that need temporary objects, which go to private scratch while the shared object store stays read-only (#2016).
