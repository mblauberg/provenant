## Fixes

### Supervisor and custody

- Lane descendants are reaped on lane settlement and on supervisor recovery after a crash or SIGKILL, tracking re-parented processes by process group, session, and recorded descendant census. Warnings are recorded when surviving descendants are killed. (#3545)
- `gc` and `doctor` report processes whose working directory is inside a settled lane's worktree or attempt directory, and `gc --apply` kills them with SIGTERM followed by bounded SIGKILL. (#3545)
- Lane briefs and test-design guidance specify that load generators and stress loops must be bounded (`timeout`) and trapped. (#3545)
