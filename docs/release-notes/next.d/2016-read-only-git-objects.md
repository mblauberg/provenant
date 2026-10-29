## Fixes
### Git and safety

- Read-only lanes can run Git commands that need temporary objects, through the shell Git wrapper, which writes them to private scratch. The shared object store stays read-only (#2016).
