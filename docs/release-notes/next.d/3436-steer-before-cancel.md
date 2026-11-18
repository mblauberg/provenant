## Fixes

### Lanes and settlement

- `provenant steer` now states, in one line, the configured
  `domains.fabric.steer.fallback` and the layer that set it before it interrupts
  the live attempt, so the restart is announced before it happens. (#3436)
- Steered lanes that settle together no longer race the launch lock: restarts
  claim slots from a shared gate spaced by the new, overridable
  `domains.fabric.steer.restart_spacing_ms` (default 2000 ms; 0 disables), and an
  applied wait names its configuration source. (#3436)
- `provenant steer --help` now names which providers keep the session across the
  restart (claude, codex, cursor, agy, opencode) and which start fresh (command,
  host-native routes). (#3436)
