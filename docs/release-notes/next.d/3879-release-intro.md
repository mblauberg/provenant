## Fixes

- A release cut no longer deletes `docs/release-notes/next.d/_intro.md`: `prepare-release` refuses an intro identical to the previous release's instead, so collation keeps passing between cuts and a stale summary cannot reach a release. (#3879)
