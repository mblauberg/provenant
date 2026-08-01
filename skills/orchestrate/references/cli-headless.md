# CLI headless reference (dated layer)

Verified locally on macOS, 2026-06-07. Model IDs, flags, auth, and safety modes drift. Always run
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

Normal answer-bearing external work uses Agent Fabric. Use headless CLIs only
for adapter/auth preflight or an explicitly recorded degraded fallback, never
as the primary worker substrate. A fallback verifier must enforce read-only or
planning mode; advisory claims require independent verification.

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
- `cursor`: `--mode ask --sandbox enabled`; current help documents ask as
  read-only, while current headless plan mode can exit without an answer.
- `kiro`: disabled by default in the dispatcher. Enable only with `CF_DISPATCH_ENABLE_KIRO=1`; no hard
  read-only mode was verified in current local help.
- `copilot`: disabled by default in the dispatcher. Guaranteed prompt-only review requires all tools
  disabled (`--available-tools=''`); repo inspection cannot currently be guaranteed read-only from local
  help.

If any adapter cannot enforce the promised safety level, log the failure and fail over. Do not silently
downgrade certification. For large prompts, prefer `--prompt-file`; enforced adapters use
stdin/file-backed input where supported to avoid shell argument limits.
Orchestrated direct-CLI runs pass `--out <run-dir>/<human-answer>`,
`--terminal-artifact <run-dir>/<terminal-artifact>.json`, and
`--reviewer-id <stable-row-reviewer-id>`, and list all three in the manifest.
The first is the human-readable provider answer. The second is the
dispatcher-owned closed JSON terminal artifact whose digest is recorded in the
dispatch receipt. The reviewer ID must be the stable ID in the consuming review
row, not a free-text alias. Omitting `--terminal-artifact` uses the declared
answer path with a `.terminal.json` suffix; the finalizer still requires the
three answer/dispatcher/semantic paths to be distinct. Omitting `--out` creates
one declared ephemeral answer for a one-shot caller to consume/remove;
dispatcher-internal prompt/raw/diagnostic temporaries are cleaned on success
and failure.

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
| `cursor-agent` | `cursor-agent --help`; `cursor-agent --list-models` | auth / workspace trust |
| `kiro-cli` | `kiro-cli chat --list-models` | credits / auth |
| `copilot` | `copilot --help`; `copilot -p "OK" --mode plan` | login / permission prompt |

## Fabric distinct-family lane

Gemini/Agy work is an Agent Fabric provider task, never a `cf_dispatch.sh`
direct-CLI route. The chair supplies a narrowed authority and budget; Fabric
records the activated adapter, model lineage, action state and recovery. Treat
the result as advisory until primary-family evidence corroborates it.

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
For a claimed commit, the chair must also run `scripts/worktree verify-claim` with the
recorded pre-dispatch base revision and accept only its clean, base-descended receipt;
missing or rejected Git evidence is not an implementation outcome.

A worker has no conversation context, so the brief carries everything: absolute path and
branch, what has already been verified so it is not redone, ordered parts, out-of-scope
paths, how to verify, and that it must not push, open a pull request or merge. Always
include the scepticism clause — *verify every claim in this brief before relying on it; if
something here is wrong, saying so is more valuable than following it* — which has
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
{"id":"...","attempt_id":"...","tool":"...","adapter":"...","model":"...","resolved_model":"...","catalog_model":"...","model_selection":"...","requested_effort":"...","effort":"...","effort_source":"...","effort_capability_source":"...","effort_substitution":"...","substitution":"...","status":"...","exit":0,"terminal_observed":true,"output_path":"...","output_sha256":"sha256:...","terminal_artifact_path":"...","terminal_artifact_sha256":"sha256:...","read_only_guarantee":"enforced|oauth_safe_mode|best_effort|prompt_only|none","orchestrator_family":"...","provider_family":"...","model_family":"...","endpoint_provider":"...","identity_source":"...","cross_family":true,"certification_eligible":true}
```

`status` is the resolver/dispatcher vocabulary, not a hand-maintained subset:
`ok`, `error`, `empty_output`, `output_write_error`, `tool_not_found`,
`auth_or_quota_error`, `unsafe_by_default`, family/orchestrator errors,
model/alias/adapter errors, capability discovery/trust/staleness errors,
effort unsupported/mismatch/unresolved errors, `same_family_forbidden`, and
`all_failed`. Consumers must tolerate a new fail-closed status as non-passing.

The clean human answer lives in `output_path`; the dispatcher-owned JSON
terminal artifact lives in `terminal_artifact_path`; the semantic worker result
is a third separately digested artifact. `terminal_observed`, `output_sha256`
and `terminal_artifact_sha256` are dispatcher-owned evidence, while
stderr/stdout noise is diagnostic only. Do not parse one tool's footer with
another tool's regex. Output files require scratch/report write permission;
that is separate from permission to edit source or evidence files.

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
