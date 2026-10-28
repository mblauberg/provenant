## Behaviour changes

- Refuse OpenCode Go models below the configurable monthly allowance floor, or with an unknown allowance, across pool and exact selection. The default floor is $60; zero disables it. Zen models are unaffected. An exact `--model opencode/opencode-go/claude-haiku-5-5` is refused by default because its allowance is $15.
