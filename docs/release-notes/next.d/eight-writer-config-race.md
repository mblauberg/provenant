## Fixes
### Git and safety

- Concurrent writer launches retry a Git config snapshot replaced during a metadata lookup, avoiding a false unsafe-directory refusal on Linux.
