## Fixes
### Lanes and settlement

- Settlement masks known credential shapes in Codex rollouts, Codex history files and Claude project sessions in the attempt's provider state, so a printed credential leaves no token-shaped plaintext in the run directory (#2303).
- A result or provider-session redaction failure no longer blocks settlement: the lane fails with the fixed `[result withheld: redaction failed]` result and reaches a terminal state, including through orphan recovery, without looping (#2303).
- Result-only orphan recovery is documented as the third redaction site: preserved bytes keep their plaintext and `redacted: false` until the attempt's evidence is pruned (#2303).
