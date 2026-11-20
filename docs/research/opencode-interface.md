# OpenCode headless interface study

Study of the OpenCode CLI as lanes drive it, so the Rust adapter
([`opencode.rs`](../../tools/front-door/crates/providers/src/opencode.rs)) and lane authors can rely on
observed behaviour. Date: 2026-11-20. Client: `opencode v2.3.2` (Homebrew
`opencode-v2`), host service `2.3.0`. Platform: macOS, Australia.

Every claim is tagged **[obs]** (observed first-hand on 2026-11-20 with the
client above), **[docs]** (official docs fetched 2026-11-20, see Sources) or
**[bin]** (strings read from the 2.3.2 binary; implementation detail, may
change). Sessions, ids and timestamps below are real captures with
credentials, the host service password and workspace ids removed.

Method: scratch directories under the session scratchpad, never the repo.
Free routes (`opencode/hy4-flash-free`, `opencode/deepseek-v4.1-flash-free`)
used freely; `opencode-go/deepseek-v4.1-flash` used for 2 calls (1 success, 1
rejected with 401). A local stub HTTP server stood in for the provider to
capture request headers and to force 401, 402, 429 and 500 responses, so no
real provider limit was provoked for those. Host config, data and state dirs
were only read, except that runs against the host profile necessarily wrote
their (archived) sessions into the host database.

## 1. Version and architecture

- **[docs] [obs]** OpenCode 2.x is client-server. One shared background service
  per user owns sessions, plugins and permissions; the CLI discovers it or
  starts it (`opencode service start|stop|restart|status|get|set|unset`).
  `--standalone` runs a private server instead; `--server <url>` attaches to a
  named server.
- **[obs]** The 2.x branch moved session storage to SQLite tables `session_v2`
  and `session_message` in `$XDG_DATA_HOME/opencode/opencode.db`, and
  credentials to a `credential` table in the same database. `auth.json` is
  now legacy: on first boot of a fresh data dir it is imported into
  `credential` (observed: a data dir holding only a symlinked `auth.json`
  gained `opencode-go` and `openrouter` rows). The `opencode` (Console
  login, label `Default`, active) credential exists only in the database.
- **[obs]** Release cadence is fast: 2.2.6 (6 Nov), 2.2.7 (10 Nov), 2.3.0
  (13 Nov), 2.3.1 and 2.3.2 (18 Nov). A 2.3.2 client attached happily to a 2.3.0
  service (no restart forced).

## 2. Headless interface

### 2.1 `opencode run`

`opencode run [flags] [message...]` **[obs: --help]**:

| Flag | Meaning |
|---|---|
| `--model, -m provider/model#variant` | Model; `#variant` selects reasoning effort etc. |
| `--format default\|json` | `default` prints the final text; `json` prints one event per line |
| `--continue, -c` | Continue the last session |
| `--session, -s <id>` | Continue this session, or create it if absent |
| `--fork` | Fork the session before continuing |
| `--worktree, -w [=name]` | Start the session in a new git worktree |
| `--agent` | Agent (default `build`) |
| `--file, -f` | Attach a file |
| `--title` | Session title |
| `--archived` | Create the session archived so it stays out of session lists (scripted and CI runs) |
| `--thinking` | Show reasoning |
| `--auto` | Auto-approve permissions that are not explicitly denied |
| `--standalone` / `--server <url>` | Private server / attach to a server |
| `--print-logs`, `--log-level` | Server logs to stderr (standalone only) |

- **Prompt:** positional words, or stdin. **[obs]** A prompt piped on stdin
  (`< file`, no positional) ran normally. The adapter's stdin delivery is valid.
- **Default format [obs]:** stdout `OK\n`; stderr gets an ANSI banner
  `> build · hy4-flash-free`. With `--thinking` stdout starts with
  `Thinking: ...`.
- **Exit codes [obs]:** 0 when the run ends normally; 1 after an `error`
  event (quota, auth, no-route, invalid reference). A run stuck in a 429 retry
  loop never exits by itself (section 3.4). No other codes seen.
- **Session resume [obs]:** `-s <id>` resumed memory ("PINEAPPLE" recalled in
  the second call; same `sessionID`). `-s <unknown id>` **creates** a session
  with that id. `-c` after an `--archived` run started a new session instead of
  continuing the archived one.
- **Docs gap [docs]:** the CLI page documents almost none of this
  (only `run`, `mini`, service flags, `debug paths`, `uninstall`, `OPENCODE_DB`);
  `--help` is the authoritative flag list. The older `--attach` flag is gone;
  use `--server`.

### 2.2 Service, `serve`, `api`

- **[obs]** `opencode service status` prints the URL when running
  (`http://127.0.0.1:<port>`) or `stopped`.
- **[obs] Discovery:** the client reads `$XDG_STATE_HOME/opencode/service.json`
  (keys: `id`, `version`, `url`, `pid`, `password`). It is keyed on the
  **state** dir only: a fresh state dir reports `stopped` (a run would start a
  new service there), while a fresh data and config dir with the real state dir
  still attaches to the host service. **[docs]** a second, private service
  config lives at `~/.config/opencode/service.json`; do not edit either file,
  use `service` subcommands. The service password is a bearer secret.
- **[obs] `opencode serve`:** `--hostname --port --cors --service --stdio`.
  A plain `serve` prints `server listening on http://127.0.0.1:<port>` and
  `server password <random>` on stdout (so scratch servers disclose their
  password on stdout; do not log it). Unauthenticated requests get 401
  `{"_tag":"UnauthorizedError"}` with `www-authenticate: Basic`. Basic auth
  user `opencode` and the password worked.
- **[obs] Attaching to a named server:** `opencode run --server <url>`. The
  password comes from `OPENCODE_PASSWORD` or `OPENCODE_SERVER_PASSWORD` (both
  worked). Without it: `{"type":"error","sessionID":"","error":{"type":"unknown","message":"Server at <url> requires a password; set OPENCODE_PASSWORD"}}`.
- **[obs] Standalone:** the client spawns a child `opencode serve --stdio
  --port 0` and hands it a random `OPENCODE_PASSWORD`; the child's logs only
  appear with `--print-logs`.
- **[docs] [obs] HTTP API:** OpenAPI 3.1 at `/docs/openapi.json` on a live server
  (173 operations in the docs reference). Useful routes: `GET /api/model`,
  `GET /api/info` (health), `POST /api/session`,
  `POST /api/session/{id}/prompt`, `GET /api/event` (volatile event stream),
  `GET|POST /api/session/{id}/permission[/{req}/reply]` (reply `once|always|reject`).
  `opencode api <operationId | METHOD path>` calls the attached server with
  stored auth: `opencode api GET /api/model` returned 412 models in 0.4 s.
  The docs mark parts of the surface experimental.
- **[obs]** `opencode session list|delete|export|import`, `opencode stats`,
  `opencode debug paths|config|agents`, `opencode auth list|export|import|switch`
  (`auth export` prints secrets; not run).

### 2.3 Permissions and non-interactive approval

- **[docs]** V2 config uses a `permissions` array of `{action, resource, effect}`
  with effects `allow|deny|ask`; the last matching rule wins; no match means
  `ask`. Actions include `read edit glob grep shell subagent skill question
  webfetch websearch external_directory` (V1 `bash` is now `shell`; V1 `task`
  is `subagent`). `external_directory` is allowed by default. `deny` always holds.
  The page documents no CLI flag for headless approval.
- **[obs]** `ask` in a headless run does not hang: the tool call fails with
  `state.status:"error"` and
  `"This non-interactive run cannot ask the user for permission, so the request was rejected. Continue without this action."`.
  With `--auto` the same `ask` rule ran the command (`tool_use(shell:completed)`).
- **[obs]** A `deny` on `shell` removes the tool: the model replied it had no
  shell tool and used `write` instead (a denial is a tool-list change, not an
  error event). So a `shell` deny does not confine a model that has `write`.
- **[obs]** The V1 object form the adapter sends (`"permission": {"*":"allow", ...}`)
  is still parsed by 2.3.2 and works.
- **[obs] Side effect:** `permission: {"*":"deny"}` (V1) or
  `[{"action":"*","resource":"*","effect":"deny"}]` (V2) makes a free-model run
  fail with `provider.free-limit` on an account that otherwise works.
  `{"*":"deny","read":"allow"}` does not. See section 5.3.

### 2.4 Configuration, paths, environment

- **[docs]** Config precedence, low to high: global `~/.config/opencode/opencode.json(c)`,
  project files from the root down to the cwd, project `.opencode/` dirs, the
  system directory (macOS `/Library/Application Support/opencode`), MDM
  preferences. Variables `{env:NAME}` and `{file:path}`.
- **[obs]** `opencode debug paths` prints data, cache, config, state, tmp, bin,
  log, repos, db. XDG_DATA_HOME, XDG_STATE_HOME, XDG_CONFIG_HOME and XDG_CACHE_HOME
  are honoured by the client. `$XDG_CACHE_HOME/opencode/bin` holds helper
  binaries (the `opencode-pty` daemon; ripgrep per the adapter).
- **[bin]** Environment variables present in the 2.3.2 binary:
  `OPENCODE_CONFIG`, `OPENCODE_CONFIG_CONTENT`, `OPENCODE_CONFIG_DIR`,
  `OPENCODE_CLI_CONFIG_CONTENT` (`cli.json`), `OPENCODE_DB` **[docs]**,
  `OPENCODE_API_KEY` (credential for the `opencode` provider),
  `OPENCODE_PASSWORD`, `OPENCODE_SERVER_PASSWORD`,
  `OPENCODE_CONFIG_PROJECT_DISABLE` (fallback `OPENCODE_DISABLE_PROJECT_CONFIG`),
  `OPENCODE_FILEWATCHER_DISABLE` (fallback `OPENCODE_DISABLE_FILEWATCHER`),
  `OPENCODE_DISABLE_CLAUDE_CODE`, `OPENCODE_DISABLE_CLAUDE_CODE_SKILLS`,
  `OPENCODE_DISABLE_CLAUDE_CODE_PROMPT`, `OPENCODE_DISABLE_AUTOUPDATE`,
  `OPENCODE_DISABLE_FREE_MODELS` **[docs]**, `OPENCODE_DISABLE_MODELS_FETCH`,
  `OPENCODE_MODELS_URL`, `OPENCODE_MODELS_PATH`, `OPENCODE_DISABLE_FFF`,
  `OPENCODE_LOG_LEVEL`, `OPENCODE_PRINT_LOGS`, `OPENCODE_WORKTREE_BASE`,
  `OPENCODE_WORKTREE_PATH`, `OPENCODE_CLIENT`, `OPENCODE_SESSION_ID`.
  The adapter's four `OPENCODE_DISABLE_*` variables still take effect through the
  `??` fallbacks.
- **[obs] The client's `OPENCODE_CONFIG_CONTENT` is ignored when attached to a
  service.** A `shell` deny passed that way did not stop the shell tool
  (`note.txt` was written) in default (attached) mode; the same content
  worked with `--standalone`.
- **[obs] The client's environment does reach attached tool execution:**
  `FOO_CLIENT=from-client opencode run ...` made `echo $FOO_CLIENT` print the value.

## 3. Event stream (`--format json`)

One JSON object per line on stdout; stderr is empty unless `--print-logs`
(standalone). Every event has `type`, `timestamp` (epoch ms) and `sessionID`.
Observed types: `step_start`, `text`, `reasoning` (with `--thinking`),
`tool_use`, `step_finish`, `retry`, `error`. Text arrives as whole parts
(`time.start` and `time.end` within 1 to 5 ms), not deltas.

### 3.1 Success (`opencode/hy4-flash-free`, 3.7 s through the service)

```json
{"type":"step_start","timestamp":1795164418507,"sessionID":"ses_e07dcdba5ffexqbvf9WruNl9xK","part":{"id":"prt_1f82331cb001jYz1dS2Gyed2kC","sessionID":"ses_e07dcdba5ffexqbvf9WruNl9xK","messageID":"msg_1f82325f70014LZfyg5DK4Yu2W","type":"step-start","snapshot":"4b825dc642cb6eb9a060e54bf8d69288fbee4904"}}
{"type":"text","timestamp":1795164418514,"sessionID":"ses_e07dcdba5ffexqbvf9WruNl9xK","part":{"id":"prt_1f82331cf001Qlvaq40dlX15pN","sessionID":"ses_e07dcdba5ffexqbvf9WruNl9xK","messageID":"msg_1f82325f70014LZfyg5DK4Yu2W","type":"text","text":"OK","time":{"start":1795164418511,"end":1795164418514}}}
{"type":"step_finish","timestamp":1795164418527,"sessionID":"ses_e07dcdba5ffexqbvf9WruNl9xK","part":{"id":"prt_1f82331df001lrD56KerIXf4So","sessionID":"ses_e07dcdba5ffexqbvf9WruNl9xK","messageID":"msg_1f82325f70014LZfyg5DK4Yu2W","type":"step-finish","reason":"stop","snapshot":"4b825dc642cb6eb9a060e54bf8d69288fbee4904","cost":0,"tokens":{"input":9276,"output":3,"reasoning":0,"cache":{"read":1792,"write":0}}}}
```

- `snapshot` is a git tree hash from OpenCode's own snapshot repo (section 6);
  `4b825dc...` is the empty tree.
- `step_finish.reason` is `stop` for the last step and `tool-calls` when the
  model called tools. `tokens` and `cost` are per step. The trivial prompt
  cost about 9.3k input tokens (system prompt plus tool schemas; 4.1k plus 7.2k
  cached on the DeepSeek free route). Go reports `cost:0` (subscription).
- The event stream never names the answering model (the adapter's
  `MODEL_ON_WIRE=false` is correct); the database does (section 7, M8).
- Mid-run `text` events occur: a run printed an apology text, then called
  `write`, then printed `DONE`. The last `text` before the final
  `step_finish reason=stop` is the answer.

### 3.2 Tool use (`echo hi > note.txt`; V2 names the tool `shell`, not `bash`)

```json
{"type":"tool_use","timestamp":1795164814442,"sessionID":"ses_e07d6d340ffeOdViAiFE2KW1QL","part":{"partID":"prt_1f8293b9c002xwYhre1ZwyEPM0","sessionID":"ses_e07d6d340ffeOdViAiFE2KW1QL","messageID":"msg_1f8292e16001g0LX2odii6vywI","type":"tool","id":"chatcmpl-tool-bbda75a2d2f65023","tool":"shell","state":{"status":"completed","input":{"command":"echo hi > note.txt"},"output":"(no output)","title":"shell","metadata":{"metadata":{"status":"completed","truncated":false,"exit":0},"content":[{"type":"text","text":"(no output)"}],"providerCall":{"executed":false},"providerResult":{"executed":false},"rawInput":"{\"command\": \"echo hi > note.txt\"}"},"time":{"start":1795164814236,"end":1795164814442}}}}
{"type":"step_finish", ... "reason":"tool-calls","snapshot":"7c88f83fe69923cf15347f92b792b31302004448", ...}
```

A `tool_use` event is emitted once the call is complete (`status` is
`completed` or `error`), not at start. The `snapshot` hash changes after a
write.

### 3.3 Errors

All are a single `error` event followed by exit 1. `error.type` is OpenCode's
own taxonomy: `provider.free-limit`, `provider.quota`, `provider.auth`,
`provider.rate-limit`, `provider.no-route`, `provider.internal`, `unknown`.

Free limit (real, `retryAfterMs` is milliseconds; see section 5):

```json
{"type":"error","timestamp":1795164449834,"sessionID":"ses_e07dc5964ffeS2h75L7eeoBd1q","error":{"type":"provider.free-limit","message":"Free usage limit reached. Connect a provider or subscribe to OpenCode Go to keep going, or switch models.","status":429,"response":{"body":"{\"type\":\"error\",\"error\":{\"type\":\"FreeUsageLimitError\",\"message\":\"Free usage limit reached. Free models work in OpenCode (https://opencode.ai); subscribe to OpenCode Go to keep going: https://opencode.ai/go\"}}"},"retryAfterMs":55430000}}
```

Expired Go API key (real, from the lane-style profile, 1.5 s):

```json
{"type":"error","sessionID":"ses_e07d61f23ffezarfdOgqb3hI9B","error":{"type":"provider.auth","message":"This API key has expired. Create a new API key in OpenCode Console and replace it in your client or secret store. Signing in again will not renew this key. ...","status":401,"response":{"body":"{\"error\":{\"type\":\"authentication_error\",\"code\":\"api_key_expired\",\"message\":\"This API key has expired. ...\"}}"}}}
```

Unknown model and bad variant (real; 0.15 s, before any provider call):

```json
{"type":"error","sessionID":"ses_e07d6b01bffeLt6VuDnVz7JGny","error":{"type":"provider.no-route","message":"Model unavailable: opencode/does-not-exist"}}
{"type":"error","sessionID":"ses_e07d6ad44ffe1zLlGso3mu8m7P","error":{"type":"provider.no-route","message":"Variant unavailable for opencode/hy4-flash-free: bogus"}}
{"type":"error","sessionID":"","error":{"type":"unknown","message":"Invalid model reference: foo"}}
```

`nonesuch/foo` (unknown provider) is also `provider.no-route`. A reference
without `/` is `unknown` with an empty `sessionID` and no status.

Provider responses forced through a stub (`providers.opencode.settings.baseURL`
pointed at 127.0.0.1; these are not real provider outputs):

| Stub response | Result |
|---|---|
| 401 `api_key_expired` | `provider.auth`, status 401, exit 1, immediate (1.7 s) |
| 402 `CreditsError` "Insufficient balance..." | `provider.quota`, status 402, immediate |
| 429 + `retry-after: 30` header | `retry` events, **no terminal error** (3.4) |
| 500 | `retry` events (10 attempts over about 80 s), then `provider.internal` status 500 |

### 3.3.1 Real upstream flake

On the free route a real run emitted
`{"type":"retry",...,"error":{"type":"provider.internal","message":"Upstream request failed: Endpoint is unavailable."}}`
and sat in retry for the whole 70 s timeout. Free routes have transient 5xx.

### 3.4 `retry` events and the 429 loop

```json
{"type":"retry","timestamp":1795164625380,"sessionID":"ses_e07d9a7cfffe7QJNzM06cCwB7n","messageID":"msg_1f8265985001LnZbB6SG6JFpUv","attempt":2,"nextRetryAt":1795164655380,"error":{"type":"provider.rate-limit","message":"Rate limit exceeded, slow down","status":429,"response":{"body":"{\"error\":{\"type\":\"rate_limit_error\",\"message\":\"Rate limit exceeded, slow down\"}}"},"retryAfterMs":30000}}
```

- A plain 429 (not the free-limit type) is retried by OpenCode itself,
  honouring the `retry-after` header (30 s became `retryAfterMs:30000`), with
  no attempt limit seen (7 requests in 150 s, process still running; the harness
  killed it). `attempt`, `nextRetryAt` (epoch ms) and the nested `error`
  describe each wait.
- With `--print-logs --log-level info` no stderr line mentioned rate, retry or
  limit during this loop. The only signal is the stdout `retry` event.
- 5xx errors are retried with growing back-off (2.3 s, 3.3 s, 7.4 s, 11.4 s ...,
  about 9 s steady) for 10 attempts, then end as an `error`.
- `provider.free-limit`, 401 and 402 are not retried.

## 4. Models, variants and effort

- **[obs]** `opencode models` prints `provider/model` lines, 0.25 s through the
  service, 1.1 s standalone. It lists only what the active credentials
  expose: the host profile lists 22 `opencode/`, 33 `opencode-go/` and 357
  `openrouter/` models; an empty profile lists only 12 `opencode/` (free) and
  no Go models; a profile holding only the stale `auth.json` lists 34 Go models
  even though that key is expired. **Listing a model proves a credential is
  present, not that it works.**
- **[obs]** `GET /api/model` (`opencode api GET /api/model`) returns richer data:
  `providerID`, `id`, `baseURL`, `variants[]`, `cost`, `limit`, `enabled`,
  `status`, capabilities. The three study routes:

| Route | Variants | Context / output | Cost per 1M tokens (in/out) |
|---|---|---|---|
| `opencode/hy4-flash-free` | `none`, `high` | 1,048,576 / 65,536 | 0 / 0 |
| `opencode/deepseek-v4.1-flash-free` | `low`, `high`, `max` | 200,000 / 32,000 | 0 / 0 |
| `opencode-go/deepseek-v4.1-flash` | `low`, `high`, `max` | 1,000,000 / 384,000 | 0.15 / 0.60 (Go catalogue price; paid Zen `opencode/deepseek-v4.1-flash` is 0.30 / 1.20) |

- **[obs]** Provider base URLs: Zen `https://opencode.ai/inference/openai/v1`,
  Go `https://opencode.ai/inference/go/openai/v1`. Each model carries an
  `x-opencode-org-id` header (the Console workspace id) when signed in.
- **[docs]** Model refs are `provider/model[#variant]`; unknown variant is a
  resolution error; the root config default drops `#variant`.
- **[obs]** The recorded variant in `session_message.data.model.variant` is
  `"default"` when none was given.
- **[docs]** Free models listed 2026-11-20: Big Pickle, Iceman Free, Inkling 2
  Small Free, DeepSeek V4.1 Flash Free, Beam Free, Flint 40B Free, LongCat 2.5
  Free (100 requests per day, the only stated limit), Hy4 flash Free, MiMo-V2.6-Flash
  Free, Nemotron 3.5 Lightning/Super VL Free, Jev 1.13 Free; all "free for a
  limited time". Zero-retention: DeepSeek V4.1 Flash Free, Hy4 flash Free,
  LongCat 2.5 Free; others retain or train. **[docs]** "Free-tier chat models
  require OpenCode", meaning the OpenCode client.

## 5. Identity and limits

### 5.1 What a request carries [obs]

Request headers captured from the client against a local stub (identifiers
masked):

- Anonymous (no credential, fresh profile): `Authorization: Bearer public`
  (13 characters; **[bin]** the client sets `apiKey:"public"` when there is no
  `OPENCODE_API_KEY`, no stored connection and no configured key),
  `x-opencode-client: cli`, `x-opencode-project: global` (or the repo's root
  commit hash when the repo has commits), `x-opencode-session`,
  `x-opencode-session-id`, `x-session-affinity` and `x-session-id` (all the
  per-run session id), `User-Agent: opencode/latest/2.3.2/cli`, `traceparent`/`b3`.
  **There is no persistent client or device id in the request.**
- Signed in (host profile): the same set plus a 46-character `Authorization`
  (the Console credential) and `x-opencode-org-id` (workspace id).

### 5.2 What the free limit is keyed on

- **[obs]** A fresh profile (any new XDG dirs, with or without a linked
  `auth.json`) gets `provider.free-limit` on the first request. Five separate
  fresh profiles over 10 minutes reported the **same reset instant**, so the
  bucket is not per profile and not per session.
- **[obs]** A profile with the host's Console credential works on the same
  models at the same minute, whether it attaches to the service or runs
  standalone with the host's data dir (`t2`: 9 s). Attaching is not what
  matters; the credential is.
- **[inference]** The anonymous bucket is shared by whatever the server can see
  of an unauthenticated caller, most plausibly the source IP. No stable header
  distinguishes anonymous clients, so a per-client key is ruled out for the
  client side; the network address could not be varied here to prove IP keying.
- **[docs]** The Zen, Go and troubleshooting pages say nothing about how free
  usage is keyed or when it resets; they only state LongCat's 100 per day.
- **No evasion:** this is documentation of how the limit behaves. A lane that
  wants the account's allowance must carry the owner's own credential (the
  host profile or service), as it would for any provider login.

### 5.3 `retryAfterMs` against the real reset [obs]

`retryAfterMs` is **milliseconds** and counts down in real time to a fixed
instant. Fresh-profile (anonymous) samples:

| Request time (UTC) | `retryAfterMs` | Implied reset |
|---|---|---|
| 08:47:29 | 55,430,000 | 2026-11-21T00:11:19Z |
| 08:47:51 | 55,408,000 | 00:11:19Z |
| 08:53:01 | 55,097,000 | 00:11:18Z |
| 08:53:06 | 55,092,000 | 00:11:18Z |
| 08:57:19 | 54,838,000 | 00:11:17Z |

Across 10 minutes the implied reset moved by 2 s (scheduling jitter), and the
chair's lane capture (56,241,000 near 08:33Z) falls on the same instant. So the
value is accurate and the Provenant hold to 00:11Z on 21 Nov is the provider's
own reset. The reset is about 15.4 hours out at 08:47Z; it is neither a rolling
hour nor tied to the model: `hy4-flash-free` and `deepseek-v4.1-flash-free`
shared it.

**A second reset exists in the same pool [obs].** An account that normally works
also received `provider.free-limit` in these cases, always with a reset of
**2026-11-21T00:00:00Z** (a UTC midnight):

| Case | `retryAfterMs` at request time | Implied reset |
|---|---|---|
| `permission {"*":"deny"}` (V1), 08:54:44 | 54,316,000 | 00:00:00Z |
| same, 08:56:02 | 54,239,000 | 00:00:01Z |
| V2 `*` deny, 08:57:13 | 54,168,000 | 00:00:01Z |
| `-s <client-chosen new id>`, 08:57:48 | 54,132,000 | 00:00:00Z |
| `-s ses_0123456789abcdefghijklmnop` | (same error) | n/a |

The same account succeeded on the same models seconds before and after with a
normal config and a server-generated session id. Both cases are reproducible
(deny twice on two models; new client-chosen id twice). The mechanism is not
established (see Unverified). The takeaway for the quota ledger is that one
pool can carry two different resets within minutes, so the newest refusal, not
a stored longer one, must set the expiry.

### 5.4 Go and paid routes

- **[docs]** Go is $10 per month (Go Plus $40, Go Teams $20 per member);
  limits are 20% of the monthly allowance per 5 hours, 50% per week (resets
  Monday 00:00 UTC) and the monthly allowance; per-model dollar limits are in
  the Console. At the limit free models still work, or Extra Usage falls back
  to Console credit. The 2.2.4 changelog raised Go limits for DeepSeek V4.1 Flash.
- **[obs]** A single tiny Go call (`opencode-go/deepseek-v4.1-flash`, host
  profile, through the service) succeeded in 9.6 s with `cost:0` and 9,619
  input tokens. The service route worked for Go with the Console credential.
- **[obs] The Go key stored in the legacy `auth.json` is expired**
  (`api_key_expired`, 401, "Signing in again will not renew this key"). The
  host works because the Console login credential is used instead. A lane
  profile built from `auth.json` alone therefore authenticates with a dead key.

## 6. Quirks and side effects

- **Start-up [obs]:** service attach 3.7 to 9.6 s per trivial run (the model
  call is most of it); three parallel attached runs finished in 5.5 s total.
  Standalone with the host config 5.4 to 9 s (plugins from the host config
  load, one printed a hook diagnostic). A fresh profile boots in under 1 s
  (62 database migrations, 8 to 13 ms) and fails in about 2 s.
- **Plugins, agents, instructions [obs]:** standalone with the host config logged
  `loading plugin` for two host plugins; `debug config` lists a global
  `opencode.jsonc` with `instructions` and `mcp`. A user config that sets
  these affects every run that reads it.
- **File watcher [obs]:** standalone logs `watcher subscribe` for the cwd's
  ancestors up to `/`, `/private` and the scratch roots; the adapter's
  `OPENCODE_DISABLE_FILEWATCHER` still applies through the fallback name.
- **Files OpenCode writes [obs]:**
  - Under data: `opencode.db`, `-wal`, `-shm`, `log/opencode.log`, `storage`,
    `snapshot/<project>/<hash>/` (a bare git dir with `objects/info/alternates`
    pointing at the repo, not inside the repo), `worktree/<root6>/<name>`,
    `shell`, `tool-output`, `repos`.
  - Under cache: `opencode/bin` (ripgrep and the `opencode-pty` daemon).
  - Under state: `service.json`, `locks/`, `model.json`, `prompt-history.jsonl`.
  - In the cwd: nothing. A plain run in a git repository left `.git` byte-for-byte
    unchanged and the status clean (checked in a primary checkout and in a
    linked worktree); files appear only when a tool writes them.
- **Project identity [obs]:** the project id is the repository's root commit
  hash. A primary checkout and its linked worktree got the same `project_id`
  with different `directory` values. A repo with no commits is `global`.
- **`-w` [obs]:** `run -w` creates a linked git worktree under
  `$XDG_DATA_HOME/opencode/worktree/<root6>/<random-name>` (detached HEAD)
  **and registers it in the primary repository** (`git worktree list` in the
  primary shows it). Lanes already own their worktree; do not pass `-w`.
- **Where tools run [obs]:** when attached, the `shell` tool executes in an
  `opencode-pty daemon` that is a child of the long-running service
  (pid chain: tool shell, daemon, service `serve --service`, launchd), not in
  the client process. Its cwd was the client's cwd. Confinement applied to the
  client does not apply to it.
- **Attached runs ignore the client's isolation [obs]:** client
  `OPENCODE_CONFIG_CONTENT` (section 2.4), `XDG_DATA_HOME` (a fresh data dir with the
  real state dir still reaches the host service and database) and the private
  config dir have no effect on the server's behaviour.
- **Session storage [obs]:** with the service, a run's session is written to the
  host database (the adapter's `session_exists` and `observe_model` read the
  lane's own `opencode.db`, which stays empty when attached).
- **Doc/behaviour gaps:** the `--attach` flag documented in 1.x is gone;
  the permission page documents the V2 array form while the V1 object still
  parses; the CLI page lists almost no flags.

## 7. Adapter comparison

The adapter assumes a standalone server with a private profile.
Evidence is from sections above. Items with a filed issue are marked.

| # | Assumption in `opencode.rs` | Observed | Suggested change |
|---|---|---|---|
| M1 | Link host `auth.json` into a private data dir for credentials. | The credential store is the database `credential` table; `auth.json` is a one-time legacy import. The linked file holds an expired Go key and no Console credential, so Go gets 401 and Zen free runs anonymous (IP bucket, already exhausted). | Covered by #3703 (host profile). Under a private profile there is no supported way to pass the Console credential short of copying database rows or `OPENCODE_API_KEY`; document that `lane` profile cannot run Zen free routes or Go. Comment added on #3703 and #3690. |
| M2 | A rate-limit retry is detectable from stderr (`rate limit exceeded ... retrying`). | 2.3.2 prints no such stderr line even with `--print-logs`. The signal is a stdout `retry` event with `error.type "provider.rate-limit"`, and the process retries forever. | Decode `retry` events; treat `provider.rate-limit` as quota or capacity at the first one and end the attempt. #3706. |
| M3 | A bad variant is reported by messages containing "unsupported/unknown variant", "variantnotfound" or "not supported/not found". | The message is `Variant unavailable for <model>: <variant>` under `provider.no-route` (the adapter maps it to `ModelUnavailable`), so the "choose a supported variant" guidance is missing and a route with a bad effort looks like an outage. | Match "variant unavailable"; validate the route's effort against `GET /api/model` `variants[]` before launch. #3707. |
| M4 | `provider.internal` is capacity only for 529 or 500 plus "overloaded". | Real free-route failures read "Upstream request failed: Endpoint is unavailable." and arrive as `retry` events for about 80 s, then `provider.internal` 500. They fall to generic `Error`. | Classify `provider.internal` 5xx after retries as capacity, and surface the retry events early. #3708. |
| M5 | `step_finish` is read only for `reason == stop`. | It also carries `tokens {input,output,reasoning,cache.read,cache.write}` and `cost` per step. A trivial run costs about 9.3k input tokens. | Report usage per attempt. #3709. |
| M6 | Sessions need no special flag. | `--archived` (2.2.6) keeps scripted sessions out of lists. Matters once lanes attach to the host service (#3703), where lane sessions land in the owner's database. | Pass `--archived`. #3710. |
| M7 | `OPENCODE_CONFIG_CONTENT`, private XDG and `allow_process_fork` shape the run. | Ignored when attached; tools then run in the service's PTY daemon outside the lane sandbox; the lane needs read access to `service.json` and loopback. | Comment on #3703. |
| M8 | `session_exists` and `observe_model` read `session_v2` and `session_message` in the lane's data dir. | Schema matches (verified against a live row: `type='assistant'`, `data.model.{providerID,id,variant}`). When attached, the rows are in the host database. | Read the host database (or `opencode session export`) under `host` profile. Comment on #3703. |
| M9 | Text part means Terminal. | Mid-run `text` followed by further tool calls was observed. The adapter's comment says success still needs exit 0, so this is safe today. | Keep, but do not use the first `text` as an answer; the answer is the text before the final `step_finish reason=stop`. |
| M10 | Env `OPENCODE_DISABLE_PROJECT_CONFIG`, `OPENCODE_DISABLE_FILEWATCHER`. | Still honoured through `??` fallback to the new names `OPENCODE_CONFIG_PROJECT_DISABLE` and `OPENCODE_FILEWATCHER_DISABLE`. | Set the new names when convenient; no urgency. |
| M11 | Runbook `providers.md`: "OpenCode uses the real HOME and XDG config". | The adapter sets private XDG data, state, config and cache dirs and links only `auth.json` and config files. | Stale text (#3703 will change the behaviour); correct it with that change. |
| M12 | Model availability from `opencode models`. | Listing follows the credential present, not the credential's validity (stale key lists 34 Go models). #3690 notes `observed_models: []` for Go. | Probe a real call or the documented `/inference/go/v1/models` endpoint (not exercised here) to establish readiness. Comment on #3690. |

The adapter's existing handling is confirmed correct for: stdin prompt, `--format json`,
the `provider.free-limit` / `provider.auth` / `provider.quota` / `provider.no-route`
mappings, `retryAfterMs` as milliseconds, empty `sessionID` on `unknown` errors, and the
database column names.

## 8. Unverified

- Whether the anonymous free bucket is keyed on IP (inferred, not varied) and
  how the 00:11Z reset is derived (rolling 24 hours from the first anonymous
  request of the day vs a fixed offset).
- Why `permission "*": "deny"` and a client-chosen new session id move an
  authenticated account into a UTC-midnight free bucket. Headers captured with the
  deny config still showed the 46-character credential, so the credential is
  attached; the server still answered with a free-limit. A server-side rule on
  request shape is the working hypothesis.
- Whether the signed-in account has a request or token quota on free models
  (none reached in about 40 requests).
- Go limits in dollars and when the 5-hour window starts (Console only); only 2
  Go calls were made.
- `--fork`, `--file` and `opencode mini`.
- `opencode-go` readiness through `https://opencode.ai/inference/go/v1/models`.
- The `quota_live::opencode` ledger (reads the same database) was not exercised.

## Sources

- https://opencode.ai/docs/cli/, /docs/server/, /docs/permissions/, /docs/config/,
  /docs/models/, /docs/zen/, /docs/go/, /docs/troubleshooting/ (fetched 2026-11-20)
- https://opencode.ai/changelog (2.2.2 to 2.3.2) and GitHub releases
  (anomalyco/opencode: v2.3.2 on 2026-11-18)
- Client `--help` output, live runs, the local database schema and the 2.3.2
  binary's strings, 2026-11-20
