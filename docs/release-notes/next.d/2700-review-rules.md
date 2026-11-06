## Fixes
### Pipelines and graphs

- Review templates report malformed findings, invalid verdicts and contradictory PASS replies as contract failures. Verification and synthesis preserve the findings and their evidence even when a brief is overridden (#2700).
- Review panels continue after a quota-limited lens or verifier when another member succeeds. Missing coverage stays partial through synthesis and cannot become a clean PASS. Shapes without synthesis include verifier verdicts (#2700).
