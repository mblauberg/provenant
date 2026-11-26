## Fixes
### Graph scheduling
- A structural pause now yields any active expansion pause, and a settled planner fallback refreshes the pause it consumed, so a checkpoint cannot be saved with an active expansion pause that does not own the graph pause. (#4106)
- A stored checkpoint whose active expansion pause disagrees with the graph pause is quarantined after one tick report with a `provenant stop -- NAME` retire step, instead of repeating the same validation error on every tick. (#4106)
