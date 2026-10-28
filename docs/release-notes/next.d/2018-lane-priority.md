## Behaviour changes

- Fabric starts lanes at configurable nice values, lowering writer priority by default. Use `--build-heavy` for build-intensive work and inspect the selected value with `lanes --json`.
