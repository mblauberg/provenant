## Fixes
### Lanes and settlement

- A writer's result no longer reports an interim message, such as "I'll stop early", as its answer when the writer went on to do more tool work. The Claude fallback text applies only when the assistant's text is the last thing before the end of the stream; otherwise the lane settles for review as having sent no final message.
- A writer stopped on its token budget commits its leftover work as `wip: stopped on budget (N files)` with the changed paths listed in the body (up to 20, then "and K more"). When the writer sent no final message, `result.md` now names the commit and paths, says the remaining work is unknown, and carries a `stopped_early` handoff.
