## Fixes

Provider processes now start with umask 077, so state they create or recreate
under the run store is private from creation rather than group and world
readable.
