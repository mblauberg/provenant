## Fixes
### CLI and install

- `provenant config apply` now refuses a patch when any proposed domain value is invalid, instead of publishing the TOML and letting the effective configuration recover it. `config schema` lists the complete typed field set, including owner bounds, enum choices and keyed settings such as quota polling intervals and project GC policy. Configuration history redacts secret-looking values nested inside arrays of objects (#2310).
