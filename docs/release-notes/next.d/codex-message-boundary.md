## Fixes
### Telemetry and providers

- A Codex or OpenCode lane whose final message holds only its handoff or verdict no longer settles `review_required` when prose came before it. Each message now starts on its own line, so new `review-council` subjects no longer fail this way. Earlier runs keep their recorded result.
