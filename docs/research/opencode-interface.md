# OpenCode headless interface

What the adapter ([`opencode.rs`](../../tools/front-door/crates/providers/src/opencode.rs), [`quota_live/opencode.rs`](../../tools/front-door/crates/providers/src/quota_live/opencode.rs)) relies on from the OpenCode CLI.
Observed first-hand with `opencode v2.3.2` on 2026-11-20; flags and environment names rechecked against the same version on 2026-11-22.
`[docs]` marks the official documentation (opencode.ai/docs), `[bin]` strings read from the binary. Use `opencode run --help` for flags; this note keeps only behaviour that code or a route decision depends on.

## Run and stream

- The adapter runs `opencode run --format json --print-logs --log-level info -m provider/model#variant`, adds `-s <id>` for a validated continuation and `--standalone` unless host access finds a running service. The prompt arrives on stdin and runs normally.
- `-s <unknown id>` creates a session with that id, so only a recorded session id may be passed.
- `run -w` creates a worktree under the OpenCode data dir and registers it in the primary repository. Lanes own their worktree and do not pass it.
- Stdout is one JSON object per line with `type`, `timestamp` and `sessionID`: `step_start`, `text`, `reasoning`, `tool_use`, `step_finish`, `retry`, `error`. Text arrives as whole parts. `tool_use` is emitted when the call completes.
- `step_finish.reason` is `stop` on the last step and `tool-calls` otherwise; it carries per-step `tokens` and `cost`. Text can appear mid-run, so the answer is the last `text` before the final `stop`. Success also needs exit 0.
- Exit is 0 after a normal end and 1 after an `error` event.
- The stream never names the answering model (`MODEL_ON_WIRE = false`). `session_message` rows of `type = 'assistant'` in `opencode.db` carry `data.model.{providerID,id,variant}`, with variant `default` when none was given. `session_exists` and `observe_model` read `session_v2` and `session_message`. Under an attached service the rows are in the host database.

## Errors

One `error` event ends the run. `error.type` is one of `provider.free-limit`, `provider.quota`, `provider.auth`, `provider.rate-limit`, `provider.no-route`, `provider.internal` or `unknown`. `retryAfterMs` is milliseconds. `decode` in the adapter maps them to typed events.

- An unknown model, unknown provider or bad variant is `provider.no-route` before any provider call. The bad-variant message is `Variant unavailable for <model>: <variant>`. A reference without `/` is `unknown` with an empty `sessionID` and no status.
- Forced through a local stub: 401 gives `provider.auth` and 402 gives `provider.quota`, both at once. A 429 with `retry-after` gives `retry` events and no terminal error, indefinitely. A 500 gives `retry` events (10 attempts over about 80 s), then `provider.internal`.
- A real free route emitted `retry` with `Upstream request failed: Endpoint is unavailable.` for a whole 70 s timeout: free routes have transient 5xx.
- A `retry` event carries `attempt`, `nextRetryAt` (epoch ms) and the nested `error`. `--print-logs` writes no rate-limit line to stderr, so the adapter's stderr `rate_limit_retry` match does not fire on this version; the `retry` event is the only signal.

Not yet handled by the adapter, each tracked in the tracker:

- `retry` events are ignored, so a 429 loop never ends the attempt (#3706).
- A bad effort is not validated against the model's `variants[]` and `Variant unavailable` gets no guidance (#3707).
- A `provider.internal` 5xx after retries is a generic error, not capacity (#3708).
- Per-step usage and cost are dropped (#3709).
- `--archived`, which keeps lane sessions out of the owner's session lists, is not passed (#3710).

## Credentials and readiness

- OpenCode stores credentials in the `credential` table of `opencode.db`. `auth.json` is imported once into a fresh data dir and can hold a stale key; the Console login exists only in the database.
- The isolated profile links only `auth.json` (`prepare`), so an isolated lane has no Console login: Go answers 401 `api_key_expired` and Zen free routes run anonymously. Host access uses the host profile and works.
- `opencode models` lists a model when a credential for its provider exists, not when the credential works. A stale Go key lists every Go model and answers 401. Readiness therefore needs a real call; [routing](../specs/routing.md#unknown-never-means-ready) treats an executable-only observation as having no offer list.
- `opencode api GET /api/model` returns `providerID`, `id`, `baseURL`, `variants[]`, `cost`, `limit`, `enabled` and `status` per model.
- An anonymous request sends `Authorization: Bearer public` and no persistent client or device id.

## Free limit

- A fresh profile gets `provider.free-limit` on its first request. Five fresh profiles over 10 minutes reported the same reset instant, so the bucket is not per profile or session; a profile with the Console credential worked on the same models at the same time. Keying on the source address is inferred, not shown.
- `retryAfterMs` counts down in real time to a fixed instant, so the value is accurate.
- The same working account also received `provider.free-limit` with a different reset (the next UTC midnight) when sent permission `"*": "deny"` or a client-chosen new session id. `{"*":"deny","read":"allow"}` did not trigger it. One pool can therefore carry two resets within minutes, which is why a newer refusal supersedes an older reset ([routing](../specs/routing.md#unknown-never-means-ready)). The adapter sends `"*": "allow"`.

## Permissions, service and environment

- A headless `ask` does not hang: the tool call fails with `This non-interactive run cannot ask the user for permission...`. `--auto` runs it. A `shell` deny removes the tool, and the model used `write` instead, so it does not confine a model that can write.
- The V1 object form the adapter sends (`"permission": {"*":"allow", ...}`) still parses on 2.3.2.
- The client finds the host service through `$XDG_STATE_HOME/opencode/service.json` (`id`, `version`, `url`, `pid`, `password`), keyed on the state dir only; `service_reachable` reads it. The password is a bearer secret.
- An attached run ignores the client's `OPENCODE_CONFIG_CONTENT`, `XDG_DATA_HOME` and private config dir. Tools run in an `opencode-pty` daemon under the service, outside the client's confinement ([supervisor](../specs/supervisor.md#permission-policy)); the client's environment does reach them.
- `[bin]` The adapter's `OPENCODE_DISABLE_PROJECT_CONFIG` and `OPENCODE_DISABLE_FILEWATCHER` work only through fallbacks for `OPENCODE_CONFIG_PROJECT_DISABLE` and `OPENCODE_FILEWATCHER_DISABLE`. `OPENCODE_DISABLE_AUTOUPDATE` and `OPENCODE_DISABLE_CLAUDE_CODE` are present.
- A plain run leaves the repository's `.git` unchanged and writes nothing to the cwd. OpenCode writes under its data dir (`opencode.db`, `snapshot/`, `worktree/`, `tool-output/`), cache (`bin/`) and state (`service.json`, `locks/`). `snapshot/` holds bare git dirs whose alternates point at the repository.

## Unverified

- What keys the anonymous free bucket and how its reset is derived.
- Why `"*": "deny"` or a new client-chosen session id moves a credentialed account into the midnight bucket. The credential header was still sent.
- Go limits in dollars and when the 5-hour window starts (Console only).
- `--fork`, `--file`, `opencode mini` and Go readiness through `/inference/go/v1/models`.

## Sources

opencode.ai/docs (cli, server, permissions, config, models, zen, go, troubleshooting), the OpenCode changelog, client `--help`, live runs and the local database schema.
