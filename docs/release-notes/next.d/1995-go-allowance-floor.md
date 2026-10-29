## Behaviour changes

- OpenCode Go models are refused when their allowance is below a configurable monthly floor or unknown, in both pool and exact selection. The default floor is $60 and zero disables it. Zen models are unaffected. For example, `--model opencode/opencode-go/claude-haiku-5-5` is refused by default because its allowance is $15 (#1995).
