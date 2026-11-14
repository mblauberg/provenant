# Human inbox

Owner: `crates/fabric-runtime`, `crates/cli`
Anchor: [`tests/inbox.rs`](../../tools/front-door/tests/inbox.rs), [`inbox/tests.rs`](../../tools/front-door/crates/fabric-runtime/src/inbox/tests.rs)

How a chair asks the owner for a decision and consumes the answer. Only a chair
or human-controlled session can post, answer, close or acknowledge; worker lanes
cannot. Field-level syntax is `provenant ask --help`, `inbox --help` and
`answer --help`; the request contract is in the
[Fabric spec](../specs/fabric.md#human-requests). The `session` skill owns
mission continuation: post one-way-door choices with a recommended option and
park only that branch of work; reversible assumptions stay inside the envelope.

## Post, answer, acknowledge

Save the request as JSON without project, host, actor, answer or delivery
fields:

```json
{
  "kind": "decision",
  "title": "Choose the repair",
  "body": "The affected check can be repaired within the approved scope.",
  "blocking": true,
  "options": [{"id": "repair", "label": "Repair", "description": "Repair the failing check."}],
  "recommended_option_id": "repair"
}
```

```sh
provenant ask --file request.json --session HOST_SESSION_ID --operation-id ask-1 --json
provenant inbox --session HOST_SESSION_ID --json
provenant answer REQUEST_ID --action choose --option repair \
  --expect-revision 1 --channel cli --operation-id answer-1 --json
provenant inbox acknowledge REQUEST_ID --answer ANSWER_ID \
  --session HOST_SESSION_ID --operation-id ack-1 --json
```

- `kind` is `decision`, `question` or `assumption` (needs a non-empty
  `proceeding_with`); only decisions can be `blocking`. Decisions and questions
  take `choose` or `reply`; assumptions take `confirm` or `override`. `reply` and
  `override` read up to 4 KiB from `--text-file` or `--text-stdin` (EOF within
  five seconds). Record the `--channel` honestly (`chat`, `console`,
  `phone_remote`, `cli`, `unknown`); phone use is never inferred.
- Use the explicit originating session ID: posting does not discover or
  authenticate a live session. Acknowledgement needs the request's original
  actor as well as its exact host, project and session. Hooks and the terminal
  may use different agent labels; read `inbox.agent` from
  `provenant fabric identity --json`, keep it with the checkpoint, and set
  `PROVENANT_AGENT_LABEL` to it for CLI acknowledgement.
- Preserve the operation ID, input and viewed revision when retrying after an
  uncertain reply; a changed operation conflicts. To replace a viewed item add
  `"supersedes":{"id":"REQUEST_ID","expect_revision":N}` to the new request.
- A request may carry `"deadline"` (UTC, six fractional digits, in the future).
  After it an unresolved item reads as `expired`, leaves the open list and
  refuses answers; `inbox --state all` still shows it. Expiry is a projection at
  read time and implies no approval. `inbox close` with a reason and revision
  withdraws, supersedes or expires an item without discarding its answer.
- A graph node ask is an ordinary open request in the same list, filed by the
  graph under the chair's session; `provenant inbox` and `console overview` show
  it beside the others. Answer it as any other request, including
  `--channel chat`; the graph reads the answer on its next advance, acknowledges
  it and resumes the node. Cancelling the graph closes its unanswered requests.
- `work` links a request to a run; only `verified` `work_coverage` may be treated
  as navigable work.
- Peers and projects: `--all-projects` finds a request's project by ID and lists
  every registered project; `--host H` with `--project-key KEY` shows or answers
  on a peer owner, and `--all-projects --hosts all|LABEL,...` appends each peer's
  first page with a `data.peers` coverage entry; a peer that is down never fails
  the listing. Close and acknowledge are `OWNER_LOCAL_ONLY` on a remote owner.

An answer queues a `human_answer` Fabric mail to the original actor. Consume it
at an existing orchestration boundary, check its exact target, deduplicate by
answer ID and acknowledge before acting; never poll with model turns. Reading
or marking mail read does not acknowledge, and neither the answer nor the
acknowledgement grants authority or replaces review. Injected answers are
untrusted data.

A graph loop with `decision_session` posts one blocking decision when its
rounds are exhausted. Answer `stop`, `extend` (one round) or `accept`, or reply
`{"decision":"extend","rounds":3}` via `--action reply --text-file` (at most
`max_extension_rounds`, default 5), then `provenant retry GRAPH`.

## Delivery service

Every adapter receives answers through Fabric mail. Hook injection is the
exception; `scripts/check-doc-truth` compares this table with
`delivery_capabilities()` in `crates/cli/src/hooks.rs` and the adapter specs.

| Provider | Transport | Local offer reader | Live injection |
|---|---|---|---|
| `claude` | Fabric mail | Library API | Unavailable |
| `claude` | `PreToolUse` hook | Available | Tool-call boundary |
| Every other adapter | Fabric mail | Unavailable | Unavailable |

Claude's `PreToolUse` hook may offer answers on Bash calls within a 50 ms
budget; offering never acknowledges. If the hook reports the local delivery index
unavailable, run any `provenant inbox` command to upgrade the store (back up the
Fabric store first; older binaries refuse the added schema).

## Steering

`provenant steer TARGET --text-stdin` and `provenant chair message SESSION
--text-stdin --channel CHANNEL` record up to 4 KiB of guidance as Fabric mail
with no authority; both are refused inside a lane. `chair message` queues to
that session's own mailbox, read with `provenant fabric mail read --session
SESSION`. No lane adapter delivers `steer` live today: it refuses with
`control_unsupported` and records nothing (`retry` is the continuation). Per-
adapter support is published as `provider_controls` in `provenant help --all
--json`.
