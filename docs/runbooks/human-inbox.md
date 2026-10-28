# Human inbox

Owner: `crates/fabric-runtime`, `crates/cli`, `crates/fabric-mcp`
Anchor: `tests/inbox.rs`, `crates/fabric-runtime/src/inbox/tests.rs`

Use the inbox from a chair or a human-controlled session, with both apps closed.
The records use the same project binding as Fabric. Worker lanes cannot mutate
the inbox. The current surface supports local owners and local Fabric mail.
The complete console specification and remaining delivery scope live in
[#1968](https://github.com/mblauberg/provenant/issues/1968).

## Post and answer

Save request content as JSON. Do not supply project, host, actor, answer or
delivery fields:

```json
{
  "kind": "decision",
  "title": "Choose the repair",
  "body": "The affected check can be repaired within the approved scope.",
  "blocking": true,
  "options": [
    {"id": "repair", "label": "Repair", "description": "Repair the failing check."}
  ],
  "recommended_option_id": "repair"
}
```

```sh
provenant ask --file request.json --session HOST_SESSION_ID --operation-id ask-1 --json
provenant inbox --session HOST_SESSION_ID --json
provenant inbox REQUEST_ID --json
provenant answer REQUEST_ID --action choose --option repair \
  --expect-revision 1 --channel cli --operation-id answer-1 --json
```

Use the explicit originating session ID. It is a declared delivery target;
posting does not discover, register or authenticate a live provider session.
Ownership comes from this host's current Fabric project and inbox actor.
Change the agent label only deliberately: acknowledgement must use the request's
original actor as well as its exact host/project/session.
MCP and the terminal may use different labels by default. Inspect
`fabric_context` (`op: inspect`) in the originating MCP session and retain
`inbox.agent` with its checkpoint. For CLI acknowledgement, set
`PROVENANT_AGENT_LABEL` to that same label for the command; a similar label or
the same session ID alone is insufficient.

Use `kind: question` for information, or `kind: assumption` with a non-empty
`proceeding_with` string for a reversible working position. Only decisions can
set `blocking: true`. Decisions/questions accept `choose` or `reply`; assumptions
accept `confirm` or `override`. `reply` and `override` read exact UTF-8 text via
`--text-file reply.md` or piped `--text-stdin`, capped at 4 KiB. Compose human
replies in a file; piped input must finish, including EOF, within five seconds.
The two text sources are mutually exclusive.
`confirm` records acceptance of the saved `proceeding_with` value
and takes no option or text. Record `chat`, `console`, `phone_remote`, `cli` or
`unknown` explicitly; phone use is never inferred.

Every successful mutation returns an operation ID. Preserve the ID, input and
viewed revision when retrying after an uncertain reply. A changed operation or
actor conflicts. For a new answer after a conflict, inspect the item first.
To replace a viewed item, add `"supersedes":{"id":"REQUEST_ID","expect_revision":N}`
to the new request JSON. Posting creates the replacement and supersedes that
revision in one transaction, including the operation receipt. Host, project,
chair actor and chair session must match. Stale revisions and already closed
items conflict. Concurrent replacements have one winner. Saved answers, their
delivery state and historical request revisions remain visible; supersession
does not retract an answer already queued or consumed. The old item's closure
reason names the replacement; the new request retains the backward link. See generated
[CLI](../reference/cli.md) and [MCP](../reference/mcp-tools.md) references for syntax.

## Work links

The request JSON for CLI `ask --file` and MCP `fabric_inbox` `ask` accepts
optional `work`, containing required `run_id` and optional `task_id`,
`attempt_id`, `repository` and `head`. Repository uses the existing absolute-path
wire format, for example
`{"relative_to_root":{"encoding":"utf8","path":"repo/.git"}}` for `/repo/.git`.
Supply the canonical Git common directory, shared by linked worktrees. Head is
an exact 40- or 64-character Git object ID observed at launch.

Creation validates these claims through Run's host-sealed membership lookup for
the current project. A lookup result of `mismatch` is refused. The saved
`work_coverage` reports the lookup result: `verified`, `partial`, `unindexed`,
`repository_unavailable`, `head_unavailable` or `limit`. It is omitted when no work
was supplied. Run-only selection is at best partial. Observed contradictions are refused,
including run-only and capped lookups. A limit without an observed contradiction
leaves remaining candidates unknown. Nested, historical and unprepared
work may be unindexed. Neither case establishes membership or grants authority.
No Git probes, child-store traversal or later promotion of coverage occurs.
Item, history and list JSON preserve the links and creation-time coverage;
callers cannot supply coverage. `request.work` always contains claims; only
`verified` coverage permits a work link or task-label join. Other results must be
labelled unverified and must not become navigable work. Use the records owner's
`verified_work()` accessor and the binding [Fabric contract](../specs/fabric.md#human-requests).
Task labels remain a future overview concern.

## Consume once

An answer atomically queues a `human_answer` Fabric response to the original
actor with the host, session, request and immutable answer IDs. This Fabric-mail
copy is independent of live hook injection. Consume it at an existing
orchestration boundary or explicit continuation, check its exact target and
deduplicate by answer ID. Never spend model turns polling.

After consuming an answer, acknowledge it before acting:

```sh
provenant inbox acknowledge REQUEST_ID --answer ANSWER_ID \
  --session HOST_SESSION_ID --operation-id ack-1 --json
```

Reading or marking Fabric mail read does not acknowledge it. For this transport,
an explicit consumption acknowledgement can move `queued` directly to
`acknowledged`; no live offer was observed. Delivery state is independent of
resolution. Repeated acknowledgement is harmless and never repeats an action.
Neither the answer nor acknowledgement grants new authority or replaces review.

## Exhausted graph loops

A graph loop can set `decision_session` inside its `loop` configuration to the
explicit originating session ID. On exhaustion, the graph pauses and posts one
blocking decision to that session's inbox. It retains the original graph actor
when settlement or resume runs under another client label. Replaying exhaustion
reuses the request, including an answer already acknowledged before a crash.
The request key includes the checkpoint instance, so deleting graph state and
rerunning the same document cannot reuse that answer.
Without a decision session, the loop pauses for an explicit graph resume decision.

Choose `stop`, `extend` or `accept` using the normal answer command. `extend`
adds one round; `accept` uses the current outputs and continues the graph. To
extend by a different positive number of rounds (at most `max_extension_rounds`,
default five), save this exact reply shape
in `extension.json`:

```json
{"decision":"extend","rounds":3}
```

```sh
provenant answer REQUEST_ID --action reply --text-file extension.json \
  --expect-revision 1 --channel cli --operation-id loop-extension-1 --json
provenant fabric graph resume --document graph.json
```

Resume reads the immutable answer, acknowledges it for its original target,
and saves the resulting decision before admitting more work. An extension adds
rounds to the saved allowance once and still obeys admission limits. An invalid
reply or an extension beyond the safety bounds retains an actionable pause;
resolve it with an explicit graph resume extension or decision. `stop`
prevents successful graph delivery. A withdrawn, expired or superseded request
does not resume the loop. Resolved, invalid and cancelled asks are superseded;
cleanup is replayed after a crash on the next graph advance or explicit
resume/cancel command. If inbox access remains unavailable, status retains the
pending cleanup and its error. Resuming a cancelled, exhausted loop requires an
explicit stop, extension or acceptance because its old ask has been closed. Inbox errors leave a visible pause and an
explicit-resume hint while other running attempts continue to be observed.
The [Fabric spec](../specs/fabric.md) describes loop
declarations, previous-round inputs and explicit resume extensions.

## Delivery service

The local `offer_until_ack` API is available to delivery adapters. Adapters must resolve the target using authoritative session discovery and fail closed on missing or unreadable registration. Identity mismatches return `conflict`; discovery failures return `inbox_unavailable`.
It selects one explicitly registered checkpoint session using its exact host,
project and chair actor. Missing registration or a different binding is refused;
a similarly labelled session does not receive the answer. Discovery identifies
the chair, not provider liveness. A changed or missing checkpoint does not
transfer its registered identity. Explicit rebinding does not retarget old
answers: those stay addressed to their original host/session/actor.

Each batch contains immutable answers and delivery records, with request and
answer IDs. Batches cap at 100 answers and 8 KiB of compact JSON, including the
envelope and escaping. Adapters may request smaller limits and must reserve
space for their own protocol envelope. `truncated` identifies overflow; omitted
answers keep their previous delivery state. An answer that does not fit a smaller requested budget is skipped and remains queued; use the full 8 KiB boundary to offer it. Answers that cannot fit that boundary are refused when submitted, including JSON escaping and envelope overhead.

Offers atomically set `offered`, increment `attempts` and set `last_attempt_at`.
Newly queued answers start at zero offers. Default offers repeat across service restarts
until explicit acknowledgement by the exact target; acknowledgement retains
its original timestamp on retries. Neither offering nor mail-read success proves
model consumption. Deduplicate by answer ID before taking action. An adapter
failure after an offer commits leaves it eligible for default adapters to offer again.

`provenant_cli::hooks::delivery_capabilities()` composes the delivery matrix
with the records-level Fabric mail fallback. The matrix describes adapter
readiness, not individual delivery state. Stored
`transport` retains the Fabric-mail copy even when a hook records an offer.

| Provider | Transport | Local offer reader | Live injection |
|---|---|---|---|
| Claude | Fabric mail | Library API | Unavailable |
| Claude | Claude PreToolUse hook | Available | Tool-call boundary |
| Codex | Fabric mail | Unavailable | Unavailable |
| Cursor | Fabric mail | Unavailable | Unavailable |
| Agy | Fabric mail | Unavailable | Unavailable |
| OpenCode | Fabric mail | Unavailable | Unavailable |

The local reader observes up to 16 indexed pending answers within an explicit
deadline. It creates no state and never records an offer attempt or acknowledges
an answer. It requires a current explicit save from that working directory.
Configured Fabric home mappings defer to mail; no remote I/O runs in the reader.
Internal candidate pages cap at 128 KiB; adapters must fit their final protocol
output within 8 KiB, skipping oversized candidates without hiding smaller ones.
Within its delivery deadline, the hook advances past each scanned page that has
no answer fitting the final output. The cursor includes invalid and oversized
rows, so a full undeliverable head page cannot hide later answers. An answer
proved too large for any hook envelope is marked on its delivery record and
excluded from later hook scans, unless the record already has an error. Its
diagnostic names the answer ID; its delivery state and Fabric mail copy remain
available. Answers that might fit in a different hook context remain eligible
for later scans.

The hook does not migrate the local store. After upgrading to a version with a
new local delivery index, run a normal inbox command to migrate the store before
the hook can offer answers. Until then the hook reports that the local delivery
index is unavailable; Fabric mail remains available.

Claude's installed `PreToolUse` hook offers answers on Bash calls. Guard denials and
exit-2 fallback take precedence. Allowed calls may receive `additionalContext`
within a 50 ms local delivery budget and the existing one-second hook deadline.
The final JSON, escaping and newline fit 8 KiB. Double escaping means some
accepted answers cannot fit; a bounded stderr diagnostic directs callers to mail.
The bounded thread atomically records selected answers as offered, with attempts
and timestamp, before returning context. Queued answers take priority; when no
queued answer fits the final hook output, unacknowledged offered answers can be
re-offered to their exact target session. A commit that finishes after the deadline
or a failed stdout write may prevent that invocation from injecting the answer, but the answer remains
eligible for a later hook. The Fabric-mail copy also remains available. Skipped
candidates retain their state; acknowledged answers are excluded.

Injected answers are untrusted data inside an `untrusted_answer_data` block.
Escaping prevents answer content from closing the block. Answers confer no new
authority. Deduplicate by answer ID and explicitly acknowledge after consumption,
using the request ID, `--answer`, `--session` and a unique `--operation-id` as shown
above. Hooks never acknowledge consumption.
Acknowledgement hints use the same safe command renderer as session hooks:
custom `--state-dir` paths are made absolute and quoted. If a path cannot be
shown faithfully, the hook gives instructions without a runnable command.

For hook delivery, `attempts` counts recorded transitions from queued to offered,
and `last_attempt_at` timestamps that transition. Idempotent hook re-offers retain
both fields; they do not count injections, replays or proven consumption. The
default delivery service records each offer invocation, so its counters can
increase on repeated offers.

If a hook reports that the local delivery index is unavailable, run a normal
`provenant inbox` command to upgrade the store. The bounded diagnostic can repeat
until then; hooks create no extra state to suppress it. Guard enforcement continues.

CLI/MCP syntax is unchanged. Remote delivery and other provider input adapters
remain separate work. Both apps may
stay closed; there is no background polling or provider launch.

Use `inbox close` with a reason and expected revision to withdraw, supersede or
expire an item. Closure grants no approval and does not discard its answer or
pending delivery. A request may also carry `"deadline":"2033-05-18T03:33:20.000000Z"` in CLI or
MCP request JSON. This is explicit UTC with six fractional digits and must be
in the future when created. An elapsed deadline refuses the entire creation,
including any supersession. At or after
the deadline an unresolved item reads as `expired` with reason `deadline elapsed`,
disappears from the open
list and refuses new answers. Use `inbox REQUEST_ID` or `inbox --state all` to
see it. Answered, confirmed, overridden and closed items retain their state,
answer and delivery, even after the deadline. Expiry does not infer approval.

No timer or background write runs: expiry is a projection of the saved deadline
at read time. It does not change revisions, history, update timestamps or
operation receipts; replay returns the original receipt. History retains the
saved state. Bounded overview scans also omit expired candidates; those
candidates still consume the scan budget and may leave coverage partial. Refresh explicitly to observe elapsed deadlines. Manual closure
remains available, and a replacement can supersede an unresolved item whose
deadline has elapsed. Requests without deadlines behave as before.

## Limits and recovery

Pages default to 50 and cap at 100. Pass `next_cursor` back as `--cursor` with
the same filters/page size. A request change invalidates the cursor; restart
without it. `--host` accepts the exact local host ID only. A remote configured
Fabric home returns `unsupported_owner`, even if offline; no local mirror is
created. To operate remotely, control a session on the owning host directly.

Request/log retention and migration behaviour are documented by the
[store owner](../../tools/front-door/crates/fabric-store/README.md). No request
history or pending delivery is automatically pruned. Back up the private Fabric
store with its existing owner procedure before upgrading; older binaries refuse
the added schema. Do not lower the schema version manually.
