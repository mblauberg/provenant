<!-- Modified for Provenant. -->

# Live iteration

Live is a write-capable design sub-mode. Before boot, the active implementation
lifecycle must already bound the project root, allowed source paths,
configuration changes, and output/cache locations. This is internal routing,
not wording the user must supply. A read-only request never enters live setup;
use read-only rendered evidence or an authorised isolated prototype instead.

Start the skill-local runtime with
`node "${AGENTS_HOME:-$HOME/.agents}/skills/ui-ux-design/scripts/live.mjs"` and use
its help and JSON event contract as the operational procedure. Keep internal
focus keys as protocol data, not a user interaction model. Validate every
project-relative source target with descriptor-bound, no-follow containment
before setup, injection, wrapping, acceptance, carbonisation, or cleanup. Stop
on ambiguity, server loss, stale session identity, malformed events, or an
expanded write set.

## First run

If boot returns `{ "ok": false, "error": "config_missing", "path": "..." }`,
create only that project-local config path. Its minimum schema is:

```json
{
  "files": ["index.html"],
  "insertBefore": "</body>",
  "commentSyntax": "html"
}
```

`files` accepts project-relative files or globs; optional `exclude` applies to
glob matches. Use either `insertBefore` or `insertAfter`, and choose
`commentSyntax` as `html` or `jsx`. Hard exclusions always remove `.git` and
`node_modules` targets. Re-run `live.mjs`; do not invent alternate config keys.

Record the session ID, project root, source baseline, server identity, event
revision, selected variant, and every changed path. Poll monotonically. A
restarted server never re-enqueues pending browser events from project journals;
retained journal state is advisory and untrusted. If the same authenticated page
still holds the complete request, reconnect live mode and click `Retry` once to
reissue it explicitly. After a reload, click `Restart` to discard the incomplete
session, wait for the agent's discard confirmation, then reselect the element;
the browser never reconstructs a partial request from the journal. Preview
selection is not lifecycle acceptance.
Preserve the action protocol and use its accept/discard/completion paths rather
than editing around them. An Accept or Discard HTTP receipt means queued only;
the browser keeps the session recoverable until the agent acknowledgement.

`server.json` is transient bearer-token state. Exclude `server.json` and
`sessions/` from version control. Annotation output uses a fresh private OS
temporary run directory and is removed with that run; keep any legacy
`annotations/` path ignored. Do not expose tokens in logs or screenshots.

## Handle fallback

On `element_not_in_source`, `element_not_found`, `file_is_generated`, or
`element_ambiguous`, the wrapper returns `fallback: "agent-driven"` without a
source write. Read the candidate ranges and rendered context, then use a more
specific element id/classes/tag/text or an explicit source `--file`. If the
element is runtime-generated, persist the selected result in its canonical
source owner rather than editing generated output. Manually place the exact
session wrapper only inside the already bounded source path; if identity or
ownership remains ambiguous, stop and report it.

## Required after accept

When acceptance emits a carbonisation (variant promotion and consolidation)
task, replace run-owned variant scaffolding with project-native source while preserving the selected result,
verify the preview, then record completion with `live-complete.mjs`. Do not poll
again until that task succeeds or is explicitly abandoned.
Reporting a carbonisation error is explicit abandonment: the accepted change
remains saved while the browser closes that session.

Source mutation uses descriptor-bound, no-follow in-place writes with
verification and process-level rollback. It is not crash-atomic: completed
edits are verified, but partial bytes can be briefly visible during a write or
remain after a process or power crash.

## Exit

Stop only the exact background-task handle returned by this run, or a run-owned
PID plus its command and start identity. Refuse broad name or pattern kills.
Verify source/configuration cleanup against the baseline and retain the journal
needed for honest recovery.

## Cleanup

Remove only run-owned transient output after source and server state are
verified. If cleanup ownership is uncertain, report the residual path and stop.
<!-- Modified for Provenant. -->
