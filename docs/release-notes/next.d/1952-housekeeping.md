## Behaviour changes

- Housekeeping runs when a lane settles and when a worktree is removed. Active supervisors pause their own builds below `gc.free_floor` and resume at the same floor. The daily GC backstop remains, and `install` quietly retires the old periodic disk-guard LaunchAgent.
- `install` and GC print aggregate counts and exceptions by default; `--json` keeps the detailed records. Disk-floor refusals name the available space, the floor and `provenant gc --apply`.
