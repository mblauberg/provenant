## Independent review

| Check | Evidence | Result | Head | Notes |
| --- | --- | --- | --- | --- |
| Independent review | `provenant output 02020202020202020202020202020202 --part result` | succeeded exit 0; route codex/gpt-6.1-terra@high | `1111111111111111111111111111111111111111` | role `lens.correctness-spec`; family openai; verdict FAIL |
| Independent review | `provenant output 03030303030303030303030303030303 --part result` | succeeded exit 0; route claude/claude-opus-5-5@high | `2222222222222222222222222222222222222222` | role `review.verify`; family anthropic; verdict PASS |
| Independent review | `provenant output 04040404040404040404040404040404 --part result` | succeeded exit 0; route codex/gpt-6.1-sol@high | `1111111111111111111111111111111111111111` | role `review.synthesis`; family openai; verdict PASS |
