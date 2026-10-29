## Fixes
### Lanes and settlement

- Settlement masks known credential shapes in Codex rollouts, Codex history files, Claude project sessions and Claude's `.txt` and `.json` tool-result files in the attempt's provider state, so a printed credential leaves no token-shaped plaintext in those transcripts. Codex SQLite databases, agy's copied history and Claude state outside `projects/` are not scrubbed (#2303).
- A result redaction failure no longer blocks settlement: the lane fails with the fixed `[result withheld: redaction failed]` result and reaches a terminal state, including through orphan recovery, without looping (#2303). A provider-session redaction failure keeps the filtered result, and the reason still reads `redaction failed; result withheld`. A cancelled, timed-out or lost lane keeps its outcome, with the redaction reason appended to its existing reason (#2303).
- One refused transcript no longer stops the scrub of the others: every entry within the bounds is scrubbed and the failure is reported at the end (#2303).
- Result-only orphan recovery is documented as the third redaction site: preserved bytes keep their plaintext and `redacted: false` until the attempt's evidence is pruned (#2303).
