## Additions
### Chair, inbox and console

- `console changes --json` reports owner-issued local change generations so console clients can refresh when state changes (#2313).
- A committed Fabric, quota, configuration, chair or log write no longer fails when its change signal cannot be published. The signal is only a hint, and a stray signal file is replaced by the next write (#2313).
