## Behaviour changes
### Accounts, quota and routing

- OpenCode Go models are now refused when their monthly allowance is unknown or below a configurable floor (default $60; zero disables it). For example, `--model opencode/opencode-go/minimax-m3.1-flash` is refused by default because its allowance is $20 (#1995).
