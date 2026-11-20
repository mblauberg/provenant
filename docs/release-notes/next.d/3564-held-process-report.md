## Fixes

### CLI and install

- `provenant gc` (preview and apply) and `provenant hosts doctor` now name processes still holding settled lane worktrees and attempt directories, with their pid, command and the cwd, executable image or open file that placed them. One time-bounded census per pass supplies the report, the caller's own session is excluded, and a path observation is never used to signal a process. (#3564)
