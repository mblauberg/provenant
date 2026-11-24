## Fixes

- `scripts/prepare-release` refuses to cut a release whose `_intro.md` repeats the previous release's intro, so a stale summary cannot ship; the intro file stays in place between cuts. (#3879)
