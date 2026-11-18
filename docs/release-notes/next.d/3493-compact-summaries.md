### CLI and output

- In compact mode, read commands print tailored summaries instead of raw JSON dumps: `templates` shows layer, name, and one-line purpose per template; `templates show` reports name, layer, nodes with their roles, and parameters with defaults; `status` tallies counts by state, shows every failed, attention, or blocked row in full, and live rows up to the cap; and `result` reports outcome, cause, verdict when present, and result path. (#3493)
