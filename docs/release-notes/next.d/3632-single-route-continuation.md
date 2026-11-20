## Fixes

### Lanes and settlement

- A writer that stops early (`stopped_early`), loses its lane token or reaches a context handoff on a node with no different route now continues on the same route in a fresh session, on the same branch with its handoff carried over, instead of pausing for the chair. A different route is still preferred when one exists, and a node `on_stop` entry still wins. The fallback is bounded by `limits.continuations` and needs progress between rounds: a round that adds no commit and leaves `remaining[]` unchanged pauses for the chair. Restart and pause reasons name the limit, its value and where it comes from. The lane preamble now says a fresh session continues from the handoff, so commit and push first. `lanes.context_handoff_tokens` text now says providers without per-call context, such as codex, never trigger it. (#3632)
