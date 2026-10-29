## Behaviour changes
### Lanes and settlement

- Housekeeping runs when a lane settles and when a worktree is removed. Active supervisors pause their own builds below `gc.free_floor` and resume at the same floor, and `install` retires the old periodic disk-guard LaunchAgent (#1952).
- `install` and GC print aggregate counts and exceptions by default, and `--json` keeps the detailed records. Disk-floor refusals name the available space, the floor and `provenant gc --apply` (#1952).
