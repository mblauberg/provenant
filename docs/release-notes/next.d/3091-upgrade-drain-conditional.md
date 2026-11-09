## Behaviour changes
### CLI and install

- Upgrades only block on running work when a release breaks state readability. Routine releases compare a compiled-in state format version during staged preflight, report in-flight graphs and lanes as warnings and proceed without requiring `--yes` or a blanket host drain (#3091).
