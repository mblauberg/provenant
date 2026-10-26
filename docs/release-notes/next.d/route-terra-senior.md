## Breaking changes

- Codex senior routing pairs GPT-6.1 Sol at medium effort with a GPT-6.1 Terra
  trial at high effort, both at weight 4 and allowing low, medium and high.
  Junior adds a Terra trial at low effort and weight 2, allowing low and medium.
  Existing junior and senior members keep their weights. Principal pairs Sol
  at high effort and weight 1, allowing medium, high and xhigh, with Claude
  Opus 5.5 at weight 1. The retired Codex principal model has no alias or
  compatibility mapping; update explicit model selections and pool overrides.
- Terra and Sol retain exact-model efforts low, medium, high, xhigh and max.
  All pools exclude max: Sol gains one AA Intelligence Index point over xhigh
  at $0.72 versus $0.39 per task. The
  [model dossier](../../model-dossier.md#solterra-benchmark-priors) records the
  AA and OpenAI-run benchmark priors, sources and 2026-10-27 read date.
  Terra high and xhigh are unmeasured; revisit both trials and the split using
  Provenant lane telemetry. Telemetry includes Terra's verified standard
  API-equivalent rates.
