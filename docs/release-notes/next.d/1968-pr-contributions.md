## Additions
### Git and safety

- `provenant pr contributions N [--json]` lists the attempts that took part in a pull request, with role, model, account and allocation. Participation is recorded at dispatch, continuation, PR creation and landing, and survives GC and worktree removal. If binding fails, PR creation and landing warn and continue; retry with `provenant pr contributions N --bind` (#1968).
