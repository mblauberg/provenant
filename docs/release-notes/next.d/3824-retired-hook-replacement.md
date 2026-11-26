## Fixes

### Hooks

- Upgrading past a retired owned hook action now transfers its ownership only to
  that action's declared replacement, rather than to every managed hook the
  release declares. Installing without `--hooks on` still registers the
  `PostToolUse` replacement of Claude's retired `PreToolUse` answer-delivery
  leaf, but a different managed leaf the user deleted by hand is no longer
  re-added. (#3824)
