## Additions
### Pipelines and graphs

- Optional `host-native` routes can run a configured Claude Code agent as a fresh lane, including the `deliver` repairer. Graphs require an ordinary fallback route, and repaired commits still pass the CI gate before review or land (#2244; see [host-native repairs](../../runbooks/presets.md#host-native-repairs)).
