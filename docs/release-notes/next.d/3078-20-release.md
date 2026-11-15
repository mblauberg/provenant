## Behaviour changes
### Lanes and settlement

- Lanes have the same tool, shell and network access as native subagents. Sandboxing is off by default and sandbox admission warns by default when a child lowers its parent’s sandbox; set `lanes.sandbox_floor` to `strict` to retain the parent sandbox or `off` to disable that advisory. Explicit path or network restrictions select an enforcing sandbox, with a warning if its backend is unavailable. Credential protections remain in place. (#3078, #3244)
