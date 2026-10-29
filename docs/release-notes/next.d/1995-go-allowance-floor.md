## Behaviour changes
### Accounts, quota and routing

- OpenCode Go models are refused when their monthly allowance is unknown or below a configurable floor, in both pool and exact selection. The default floor is $60 and zero disables it; Zen models are unaffected. For example, `--model opencode/opencode-go/claude-haiku-5-5` is refused by default because its allowance is $15 (#1995).
