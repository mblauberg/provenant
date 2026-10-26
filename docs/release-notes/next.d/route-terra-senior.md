## Breaking changes

- Codex senior routing now uses GPT-6.1 Terra at high effort and weight 4.
  GPT-6.1 Sol moves to principal at high effort and weight 1, alongside Claude
  Opus 5.5 at weight 1. The retired Codex principal model is removed with no
  alias or compatibility mapping. Update explicit model selections and pool
  overrides to use Terra for senior work and Sol for principal work.
- Terra and Sol support exact Codex efforts low, medium, high, xhigh, max and
  ultra. The senior pool allows low through xhigh; principal Sol allows medium
  through ultra. Telemetry includes Terra's verified standard API-equivalent
  rates.
