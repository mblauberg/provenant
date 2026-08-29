<!-- Modified from Impeccable for this harness; see the repository THIRD_PARTY_NOTICES.md. -->

# Live iteration

Live is a write-capable design sub-mode. Before boot, the active implementation
lifecycle must already bound the project root, allowed source paths,
configuration changes, and output/cache locations. This is internal routing,
not wording the user must supply. A read-only request never enters live setup;
use read-only rendered evidence or an authorised isolated prototype instead.

Start the skill-local runtime with `node <skill-root>/scripts/live.mjs` and use
its help and JSON event contract as the operational procedure. Keep internal
focus keys as protocol data, not a user interaction model. Validate every
project-relative source target, including realpath containment, before setup,
injection, wrapping, acceptance, carbonisation, or cleanup. Stop on ambiguity,
server loss, stale session identity, malformed events, or an expanded write
set.

Record the session ID, project root, source baseline, server identity, event
revision, selected variant, and every changed path. Poll monotonically; on
restart, resume only when the journal and current source match the recorded
session. Preview selection is not lifecycle acceptance. Preserve the action
protocol and use its accept/discard/completion paths rather than editing around
them.

`server.json` is transient bearer-token state. Exclude `server.json`,
`sessions/`, and `annotations/` from version control. Do not expose tokens in
logs or screenshots.

## Required after accept

When acceptance emits a carbonisation (variant promotion and consolidation)
task, replace run-owned variant scaffolding with project-native source while preserving the selected result,
verify the preview, then record completion with `live-complete.mjs`. Do not poll
again until that task succeeds or is explicitly abandoned.

## Exit

Stop only the exact background-task handle returned by this run, or a run-owned
PID plus its command and start identity. Refuse broad name or pattern kills.
Verify source/configuration cleanup against the baseline and retain the journal
needed for honest recovery.

## Cleanup

Remove only run-owned transient output after source and server state are
verified. If cleanup ownership is uncertain, report the residual path and stop.
