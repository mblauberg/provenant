## Fixes

### Lanes and settlement

- Refused lane launches record a typed terminal cause plus the fixing command on the attempt and in `terminal-pending.json`, eliminating opaque refusals in `result` and `wait`. Unlaunched attempt settlement and queued proposal refusals settle with their typed cause instead of missing terminal causes. (#3544)
- Lane Git object import runs with `advice.graftFileDeprecated=false`, and git advice lines are filtered from stderr so advice messages are never surfaced as Provenant warnings. (#3544)
