## Behaviour changes
### Accounts, quota and routing

- OpenCode Go models are now refused when their monthly allowance is unknown or below a configurable floor (default $60; zero disables it). For example, `--model opencode/opencode-go/claude-haiku-5-5` is refused by default because its allowance is $15 (#1995).
