## Fixes

### Session and hooks

- The session-start digest now names only custody this chair holds in the hook's project. Unattended items open to adoption and other projects' rows collapse to one `Unattended: N (M this project) — provenant status --adoptable` line instead of filling the injection, and `status --adoptable` lists those rows for the chair to inspect. `session.digest.scope = project|all` (default `project`, set under `[knobs]` or with `provenant config set`) opts back into the cross-project view; the effective scope and its layer appear in the line. (#4026)
