## Behaviour changes

### Pipelines and graphs

- Expensive macOS jobs now wait for the cheap Linux `rust-lint` job before they start. A change that fails the compile gate skips the macOS archive, lint and release jobs instead of claiming a scarce runner; a macOS suite still runs when the gate is skipped because the rust suite is covered by green evidence. The dependency is declared in `config/ci-suites.toml` and checked by `scripts/check-ci-suites`. (#4025)
