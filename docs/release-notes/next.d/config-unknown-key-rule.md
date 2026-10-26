## Fixes

- An unknown key inside the `install`, `gc` or `retention` config sections now
  reports schema rule `unknown_key` at the key's location instead of `type`. A
  wrong value type still reports `type`.
