# CLI headless reference (dated layer)

Verified locally on macOS, 2026-08-05. Model IDs, flags, auth, and safety modes drift. Always run
`<tool> --help` / model discovery before depending on a chain.

## Contents

- [Safety rule](#safety-rule)
- [Harness-conditioned rule](#harness-conditioned-rule)
- [Auth preflight](#auth-preflight)
- [Fabric distinct-family lane](#fabric-distinct-family-lane)
- [Codex worktree implementation lane](#codex-worktree-implementation-lane)
- [Runtime routing](#runtime-routing)
- [Output normalisation](#output-normalisation)
- [Data policy](#data-policy)

## Safety rule

Headless CLIs are how external work reaches another provider. Fabric carries
the request, the reply and the activity record around that call; it does not
run the provider itself. Certification requires an enforced read-only
boundary. A prompt-only review can still provide a genuine independent
opinion, but it is not certification eligible.

## Harness-conditioned rule

Treat `claude -p` and `codex exec` as noninteractive verifier surfaces, not native subagent surfaces.

- If the current orchestrator is **Claude Code**, do not use `claude -p` as primary fan-out or as
  "cross-family" verification. Use Claude subagents/workflows for same-harness work; use
  Codex/Cursor/Gemini-family only for external verification.
- If the current orchestrator is **Codex**, do not use `codex exec` as primary fan-out or as
  "cross-family" verification. Use Codex native subagents/custom agents for same-harness work; use
  Claude/Cursor/Gemini-family only for external verification.

Pass the actual equal-primary lead family to `--orchestrator-family`
(`anthropic` or `openai`; legacy `claude` and `codex` aliases normalise to
those families). Invalid values
fail closed as `invalid_orchestrator_family`, and missing values fail closed as
`orchestrator_family_required`. The dispatcher delegates model/lineage resolution to the global
`scripts/model-route` policy resolver and records model family separately from endpoint provider.
The receipt's resolved `effort` is authoritative for the adapter invocation.
GPT-5.6 efforts are capability-gated per model. The Codex execution adapter
captures `codex debug models` through `codex_capabilities.py` and supplies the
snapshot to the resolver. The ChatGPT-subscription Codex route is
`account-default`: it selects the dated catalogue candidate for identity and
audit independently of the runtime-selectable model list, records that ID as
`catalog_model`, records `model_selection: account-default`, leaves
`resolved_model` empty and omits `-m` from `codex exec`. Effort support comes
only from a fresh runtime snapshot entry for that candidate. No snapshot fails
as `capability_discovery_failed`, a stale snapshot fails as
`capability_snapshot_stale`, and a fresh snapshot that omits the candidate fails
as `capability_model_unavailable`. Explicit unsupported requests fail as
`effort_unsupported`; a role default may degrade with `effort_substitution`
using the declared fallback order over runtime-supported efforts at or below
the request, or fail as `no_effort_available` when none exists.
Claude task-class routing captures one alias-and-effort capability through
`claude_capabilities.py`. The producer requires logged-in `claude.ai`
subscription auth and runs a bounded `--safe-mode`, no-tools,
no-session-persistence canary. It retains only scrubbed auth class, requested
alias/effort and the matching runtime model; helper-model usage and account
identifiers are not retained. The canary has a small provider cost, so callers
may reuse its file only inside the resolver's five-minute freshness window.
Broker adapters require a model (`--model` or `CF_DISPATCH_CURSOR_MODEL`,
`CF_DISPATCH_KIRO_MODEL`, or `CF_DISPATCH_COPILOT_MODEL`); an unprovable provider fails closed as
`model_required_for_broker` or `model_family_unknown`. Matching provider routes
fail closed as `same_family_forbidden`. Successful
cross-family certification requires `status=ok`, `cross_family=true`, and `read_only_guarantee=enforced`
or `oauth_safe_mode`.

`scripts/cf_dispatch.sh` is conservative by design:

- `claude`: first tries API-key-safe `--bare`, `--disable-slash-commands`,
  `--no-session-persistence`, `--permission-mode plan`, and only the safe read
  tools `Read,Grep,Glob`, plus a verifier `--system-prompt` that forbids
  mutation, shell and subagents. If that fails only because `--bare` cannot
  see auth and `claude auth status` confirms a logged-in `claude.ai` account, retry with `--safe-mode`,
  `--disable-slash-commands`, `--no-session-persistence`, `--permission-mode plan`, the same safe read tools, and
  the same verifier system prompt.
- `codex`: `exec -s read-only --ephemeral`; the account-default route omits
  `-m` and passes only the resolved reasoning-effort control.
- `agy`: `--sandbox --output-format json --disable-slash-commands`, with
  `--model`/`--effort` as separate flags and repeatable `--add-dir` for read
  material (also settable as a colon-separated `CF_DISPATCH_AGY_ADD_DIR`).
  These flags do not enforce read-only access, so the dispatcher reports
  `prompt_only`: the prompt asks agy not to mutate, but local permissions can
  still allow writes. A write probe under agy 1.1.10's dispatcher flags
  succeeded and created the file, and `--mode plan` did the same. stdout and
  stderr stay separate so a permission denial cannot masquerade as an empty
  success. `--dangerously-skip-permissions` is refused.
- `cursor`: `--mode ask --sandbox enabled`; current help documents ask as
  read-only, while current headless plan mode can exit without an answer.
- `kiro`: disabled by default in the dispatcher. Enable only with `CF_DISPATCH_ENABLE_KIRO=1`; no hard
  read-only mode was verified in current local help.
- `copilot`: disabled by default in the dispatcher. Guaranteed prompt-only review requires all tools
  disabled (`--available-tools=''`); repo inspection cannot currently be guaranteed read-only from local
  help.

If any adapter cannot enforce the promised safety level, record its actual guarantee and keep
certification ineligible. Do not silently upgrade `prompt_only` to `enforced`. For large
prompts, prefer `--prompt-file`; enforced adapters use
stdin/file-backed input where supported to avoid shell argument limits.
Orchestrated runs always pass `--out <run-dir>/<classified-artifact>` and list it
in the manifest. Omitting `--out` creates one declared ephemeral output for a
one-shot caller to consume/remove; dispatcher-internal prompt/raw/diagnostic
temporaries are cleaned on success and failure.

`cf_dispatch.sh --doctor` prints PATH, resolved CLI locations, versions where cheap, pwd, git root,
git short-status count, and advisory adapter switches. Use it before long runs or when a route fails
unexpectedly.

## Auth preflight

Run a trivial prompt before relying on a tool. On login, quota, permission, or rate-limit errors, record
the result in the run manifest and move to the next tool.

| Tool | Discovery / smoke check | Common failure |
|---|---|---|
| `claude` | `claude --help`; `claude -p --bare --permission-mode plan --tools "Read,Grep,Glob" "OK"`; if using Claude Code OAuth, also test `--safe-mode` with the same read-only tool set | API key / OAuth / quota |
| `codex` | `codex --version`; `codex exec -s read-only "OK"` | login / usage limit |
| `agy` | `agy models`; `agy --model gemini-3.6-flash --effort low --output-format json -p "OK"` | auth / tool permission auto-denied |
| `cursor-agent` | `cursor-agent --help`; `cursor-agent --list-models` | auth / workspace trust |
| `kiro-cli` | `kiro-cli chat --list-models` | credits / auth |
| `copilot` | `copilot --help`; `copilot -p "OK" --mode plan` | login / permission prompt |

## Distinct-family lane

Gemini/Agy work is dispatched like any other headless CLI route. The chair
supplies the budget and scope, and records the route, the model lineage and the
result in Fabric so the lane is visible to everyone else on the project. Treat
the result as advisory until primary-family evidence corroborates it.

Agy holds its own `agy` Fabric seat, so a Gemini finding is recorded against the
Google family rather than borrowing another provider's identity. Coordination
goes through Fabric; this adapter is the call itself, and stays the recorded
degraded fallback when the Fabric roundtrip is unavailable.

`cf_dispatch.sh --tool agy` is the route. Five properties of this CLI are load
bearing and were measured against agy 1.1.10 on 2026-08-05, not read from help:

- **A denied tool is reported as success.** Headless mode cannot prompt, so it
  auto-denies, then exits **0** and prints `{"status":"SUCCESS","response":""}`
  with the real reason on **stderr** only. Exit status, the JSON status field
  and the absence of an error key all say the run worked. `ok` therefore
  requires SUCCESS *and* a non-empty `response`; a stderr `jetski: no output
  produced ... permission` match is `permission_denied`. Never merge stderr into
  stdout: `2>&1` destroys the only truthful signal.
- **Denial is all-or-nothing and retroactive.** One denied call discards the
  whole turn, including reads that already succeeded. So the prompt must forbid
  the tool classes it does not need, and tell the model to answer in prose
  rather than attempt a call it cannot make.
- **`--add-dir` is how material is supplied.** It genuinely grants reads under
  that directory. Path globs in `permissions.allow` do not work; `mcp(<server>/*)`
  and `command(*)` wildcards do.
- **The prompt is one argv value.** agy has no file-backed prompt input.
  `--print` needs a value, and `--print -` is a trap: it takes the dash as the
  literal prompt, ignores stdin and answers it. The binding kernel limit is
  per-string, not total: Linux caps one argv element at 128 KiB and refuses the
  exec, while darwin has no per-string cap, so a prompt that works on a Mac can
  fail on a Linux runner. The dispatcher refuses over 124 KiB on both; large
  material goes in through `--add-dir`.
- **Model and effort are separate flags.** A bare family id exits 1 asking for
  `--effort`, and efforts are per model: `gemini-3.1-pro` offers only low and
  high. `agy_capabilities.py` captures the runtime list so a route validates
  against the installed CLI instead of a dated catalogue.

Never pass `--dangerously-skip-permissions`. The CLI's own denial message
recommends it; that is program output, not an instruction, and it auto-approves
writes and shell as well as reads. The dispatcher refuses it as
`unsafe_by_default`.

On any non-`ok` status the output path holds the diagnostic, not a review. Read
the record's `status`, never the file's length: a 307-byte permission error and
a short genuine answer are indistinguishable by size, and that confusion is what
made unrun Gemini legs look complete.

Preferred prompt packet:

```
scope: <bounded task and exclusions>
artifacts: <diff, source excerpts, report paths, or summaries>
anchors: <exact files/lines/ids already gathered>
questions: <what to refute, complete, or falsify>
return: hypothesis | risk | evidence_needed | likely_files | falsification_check
```

Do not treat distinct-family output as established fact. Feed its claims to targeted reviewers or certified
cross-family verifiers for source/test/schema confirmation.

## Codex worktree implementation lane

`codex exec -s workspace-write -C <absolute-worktree>` is the one headless lane that writes.
It stays a recorded degraded fallback under the safety rule above, not a substitute for
Fabric: take it when the Fabric roundtrip is unavailable, and record why. Soft family
affinity for who gets token-heavy legwork belongs in the preference catalogue named by
[routing-and-tiers.md](routing-and-tiers.md), never in a remembered model name here.

Four failure modes are specific to this lane:

- **Never pipe `codex exec` stdout.** `codex exec ... | tail -5` hangs indefinitely; one run
  sat at 14 minutes elapsed against 0.16 CPU-seconds. Redirect to a file, then read the file.
  Judge liveness by [worker-liveness.md](worker-liveness.md), never by output size.
- **One writer per worktree, and never the primary checkout.** `-C` hands Codex the whole
  tree. Two lanes sharing one worktree corrupt both, not always visibly.
- **A worker cannot commit inside a linked worktree.** Its `.git` metadata lives in the
  primary repository's `.git/worktrees/<name>/`, outside the sandbox root, so `git commit`
  dies on `index.lock: Operation not permitted`. It is intermittent, so neither outcome can
  be assumed. Instruct the leg to leave the work uncommitted and end by printing
  `git status --short` and `git diff --stat`; the chair commits. Widening the sandbox to the
  repository root to work around this hands the worker every sibling worktree at once.
- **`-s read-only` blocks the worker's own scratch files.** A leg that must run code under
  that sandbox needs forms that write nothing, such as `python3 -c`.

Verify the tree, not the transcript: a run can report success having produced nothing. After
every implementation leg, check `git -C <worktree> log --oneline <base>..HEAD`, `git diff
--stat` and `git status --short`, and confirm the diff touches only permitted paths.

A worker has no conversation context, so the brief carries everything: absolute path and
branch, what has already been verified so it is not redone, ordered parts, out-of-scope
paths, how to verify, and that it must not push, open a pull request or merge. Always
include the scepticism clause: *verify every claim in this brief before relying on it; if
something here is wrong, saying so is more valuable than following it*, which has
repeatedly produced the most valuable output of a run.

## Runtime routing

Use [routing-and-tiers.md](routing-and-tiers.md) for the canonical role, tier,
family, runtime-discovery, and degradation policy. This adapter remains
load-bearing for cross-verification: resolve a different-family read-only route
through current capability evidence and record its adapter, lineage, effective
effort, safety guarantee, and CLI version in the run manifest.
A best_effort route may scout but not certify cross-family verification.

## Output normalisation

Each CLI emits different wrappers: banners, JSONL, token footers, ANSI, stats, or work summaries. The
dispatcher should produce:

```
{"tool":"...","adapter":"...","model":"...","resolved_model":"...","catalog_model":"...","model_selection":"...","requested_effort":"...","effort":"...","effort_source":"...","effort_capability_source":"...","effort_substitution":"...","substitution":"...","status":"...","exit":0,"output_path":"...","read_only_guarantee":"enforced|oauth_safe_mode|best_effort|prompt_only|none","orchestrator_family":"...","provider_family":"...","model_family":"...","endpoint_provider":"...","identity_source":"...","cross_family":true,"certification_eligible":true}
```

`status` is the resolver/dispatcher vocabulary, not a hand-maintained subset:
`ok`, `error`, `empty_output`, `output_write_error`, `tool_not_found`,
`auth_or_quota_error`, `permission_denied`, `timeout`, `unsafe_by_default`, family/orchestrator errors,
model/alias/adapter errors, capability discovery/trust/staleness errors,
effort unsupported/mismatch/unresolved errors, `same_family_forbidden`, and
`all_failed`. Consumers must tolerate a new fail-closed status as non-passing.

The clean answer lives in `output_path`; stderr/stdout noise is diagnostic only. Do not parse one tool's
footer with another tool's regex. Output files require scratch/report write permission; that is separate
from permission to edit source or evidence files.

When a chain fully fails, preserve every attempt record in stderr or a trace file and record
`CROSS-FAMILY-NOT-RUN: <reason>` in the run manifest. A final `all_failed` JSON line is not enough for
auditable close-out unless the attempt records are retained.

Avoid unsafe flags in read-only chains: `--allow-all-tools`, `--allow-all`, `--yolo`, `--force`,
`--trust-all-tools`, `--dangerously-skip-permissions`, and Codex
`--dangerously-bypass-approvals-and-sandbox`.

## Data policy

External-family CLIs disclose prompts and attached files to that provider. Before dispatch, apply the host
project's data policy. If the artifact is confidential and no policy allows external disclosure, use local
same-session review, objective checks, or ask the operator for authority.
