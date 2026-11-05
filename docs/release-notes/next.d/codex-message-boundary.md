## Fixes

- A Codex or OpenCode lane whose final message holds only its handoff or verdict no longer settles `review_required` when prose came before it. Each message now starts on its own line, which also repairs `review-council` subjects that failed this way.
