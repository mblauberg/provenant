## Fixes

### CLI and install

- `provenant gc` (preview and apply), `provenant hosts doctor` and `provenant worktree doctor` now name every process still holding a settled lane worktree or attempt directory, with its pid, command and the cwd, executable image or open file that placed it. One time-bounded census per pass supplies the report; the caller's session and verified ancestors are excluded, only terminal, custody-released attempts contribute, and a path observation is never used to signal a process. A cut-short census warns that results may be missing, and locally produced reports cap the row list at 128 with a "more not shown" line. (#3564)
