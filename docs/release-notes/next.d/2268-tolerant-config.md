## Behaviour changes
### CLI and install
User configuration now warns on unknown keys and invalid optional values,
preserving valid settings and reporting their sources. `provenant config check`
lists all warnings with their file and key. Syntax, size, schema version,
credentials, Git-boundary and authority checks remain strict, as do bundled assets.

Persisted state accepts added fields across worktree builds. Agent JSON requests
ignore unknown fields with source-qualified warnings. PR templates gain missing
standard sections automatically. Lane admission now defers at critical memory
pressure by default; `domains.fabric.lanes.memory_pressure_threshold` accepts
`critical`, `warning` or `never`. Memory and disk floors still apply.
