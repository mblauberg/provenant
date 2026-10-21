# Provider CLI quirks

Durable behaviours of the external CLIs behind Fabric lanes. Model ids, efforts
and flags change often: discover them at runtime (`provenant routes`,
`fabric_adapters`, `agy models`) and never trust a written slug. Mechanics live
in `skills/orchestrate`; this page records the traps.

## Any provider

- A fast, empty or very short lane result is a defect signal, not a clean pass.
  Check the receipt status and the transcript before reporting a verdict, and
  keep a fallback verifier ready.
- A model's claims need an objective re-check against the source. Cross-family
  reviewers raise wrong findings as well as right ones.
- Do not give a read-only or sandboxed lane write tasks outside its worktree.
  Make the complete final response the deliverable and have the chair persist it.
- Avoid spending a family's models through a tight-quota seat (for example GPT
  or Claude through a Cursor seat): limits burn fast. Configured gateway routes,
  such as agy-hosted Claude or Kiro, are fine; the receipt records the host.
- Prefer a native subagent for your own family and a headless CLI for another.
  Never self-deploy through your own headless CLI.
- Strip ANSI noise from CLIs that emit it before parsing their output.

## agy (Antigravity, Gemini family)

- Headless `-p` cannot prompt for tool permission. A denied call exits 0 with
  `{"status":"SUCCESS","response":""}` and the only real signal on stderr. One
  denial discards the whole turn, reads included. Judge the dispatcher's record
  status, never exit code or output size, and keep stderr separate.
- `--add-dir` grants reads. Wildcard permission rules work for some categories
  (`mcp`, `command`) but not `read_file`/`write_file` paths.
- `--model` and `--effort` are separate flags; `--effort` is mandatory and
  efforts are per model (some offer only low and high). There is no `-m` alias.
  An unknown slug can silently fall back to a cheaper model: confirm the
  reported model. Re-verify older agy evidence gathered without explicit
  `--model` and `--effort` flags: earlier `-p` releases ignored them.
- The prompt is one argv value. `--print -` treats the dash as the literal
  prompt. Linux caps one argv string at 128 KiB, macOS does not, so a large
  brief works on a Mac and fails on a Linux runner: keep briefs under about
  124 KiB and put bulk material behind `--add-dir`.
- Large or code-heavy prompts have hung with zero output, and `--print-timeout`
  is not honoured. Send compact per-region prompts and wrap the call in an
  external watchdog. Treat agy output as advisory until checked independently.
- Quota errors name the reset window and may cover one hosted-model pool:
  read the window rather than retrying, and switch to another available pool.
  Probe quota with the model the real lane will use.

## Codex

- Per-call startup overhead is heavy: do not use it for trivial calls.
- A linked-worktree sandbox can block commits and writes outside the worktree.
  Let it return findings inline or write inside the worktree, and let the chair commit.
- A wrapper can report "completed" while the underlying task never wrote its
  output (for example a broker busy with another workspace). Verify the file exists.
- On the degraded direct-CLI path only, a backgrounded `codex exec` hangs
  unless stdin is closed (`< /dev/null`). Fabric sets stdin itself.

## Claude CLI

- Sandboxed lanes run with `~/.claude.json` read-only, including atomic rewrite
  and backup siblings. Reads follow a user-managed symlink normally. A live
  read-only lane on 2026-10-22 started and answered with the file read-only
  and left it unchanged. If a later Claude Code release cannot start that
  way, reopen #927 rather than making the file writable.
- Seatbelt checks `file-write*` on both ends of a hard link: a write deny on
  the existing file or on the new name blocks `ln`. Under the lane profiles'
  default write deny, a lane can only hard-link files it can already write, so
  local clones and `cp -al` work inside writable roots while protected provider
  state cannot be linked out. Keep this in mind before widening a write grant:
  anything inside the grant becomes linkable.
- The in-session Agent tool picks a model tier but has no per-call effort knob;
  headless `claude -p --model <m> --effort <e>` does. `--bare` gives a clean
  minimal context for an independent verifier.

## OpenCode (v2)

Fabric drives OpenCode v2 only (tested on 2.0.30). A v1 binary fails planning with
a typed error ("upgrade OpenCode to v2: `opencode upgrade`"); `opencode --version`
is checked once per process.

- **Argv:** `opencode run --standalone --format json --auto --print-logs --log-level error
  [-s SESSION] [--model provider/model#variant] PROMPT`. v1 flags `--dir` and `--variant` do
  not exist in v2 (help, exit 1); the working directory is the process cwd, and effort is a
  variant suffix on the model id. The log level is lowercase only (`ERROR` is rejected).
- **Always `--standalone`.** The default mode talks to a shared background service that ignores
  the per-dispatch `OPENCODE_CONFIG_CONTENT` permissions: a read-only model wrote a file with
  edit and bash denied. Standalone honours them and leaves no daemon behind. The same flag is
  used for `opencode session export <id>`, which replaces v1 `opencode export` and supplies the
  observed model (`info.model`) and session for resume.
- **Stdin must be null.** `opencode run` waits on a non-tty stdin; Fabric spawns it with
  `/dev/null`. A hand-run backgrounded call without that hangs or exits 0 with no output.
  Exit 0 with no assistant text is classified `empty_output`.
- **Models and effort.** Shorthands (`deepseek`, `glm`, `deepseek-free`, `glm-free`,
  `deepseek-paid`, `glm-paid`, ...) resolve through the catalogue. `model#variant` or `--effort`
  is sent as the variant; variants are not listable, so an unsupported one returns a typed
  `invalid_input` ("Variant unavailable for ..."). Catalogued models without an `efforts` list
  ignore `--effort` with a note. A bare uncatalogued name is resolved against the live
  `opencode models` listing: exact tail match wins, a unique prefix match runs with a note, several
  matches give `model_ambiguous` (candidates listed), none gives `model_not_found` (closest names
  and a fix). Prefixed ids (`opencode/`, `opencode-go/`, `openrouter/`) in the live listing run with
  a note; an id the provider rejects returns `model_unavailable` ("list every id with `opencode models`").
- **Errors** arrive as stdout events `{"type":"error","error":{"type","message"}}`:
  `provider.no-route` (unknown model or variant), `provider.quota` (free-tier throttle, 429,
  `FreeUsageLimitError`; classified `rate_limited`), `unknown` ("Invalid model reference").
  Stderr carries INFO logs even at `--log-level error`. Reasons use the first WARN/ERROR
  line, or an exit diagnostic when none exists; INFO/DEBUG logs are excluded. Sandbox EPERM
  without a final answer is `permission_blocked` (`sandbox_denied`), neutral for route health.
- **Output before exit.** Final assistant-step text goes to `result.md`, without earlier working
  narration or stderr. Exit 1 after that text, without a structured provider failure,
  is `ok` with signature `exit_after_output` and a warning. The adapter exit is normalised to
  zero for dispatch; `evidence.exit` keeps the real process exit. Route health records the
  successful outcome and warning. Other exits, such as 2 (usage) and 124 (timeout), stay failures
  that keep their output. A later tool call or new step invalidates intermediate
  text as completion evidence. Stream `step_finish` usage, including cache and reasoning
  tokens, remains in receipt `context` fields with `usage_source: observed`.
- **Read-only sandbox.** The macOS profile keeps the blanket read deny on HOME for OpenCode and
  re-allows only what v2 needs to start: listings (not contents) of every ancestor of the cwd, of
  home, `~/.claude` and `~/.agents`, plus the instruction files `AGENTS.md`, `CLAUDE.md` and
  `HARNESS.md` in each ancestor and `~/.agents/AGENTS.md` and `~/.claude/CLAUDE.md`. Absolute
  `instructions` entries in the global `opencode.json(c)`, such as the installer's
  `~/Repos/provenant/HARNESS.md`, are granted with their ancestors' listings unless they are globs,
  URLs or credential paths, so a lane in another project still starts. v2 realPath()s those at
  startup and follows `@imports`; without them the server fails with `UnexpectedStatus: 500` or
  `Instruction initialization blocked` (EPERM on `lstat ~/.agents/AGENTS.md`). The provider state
  grant stays: `~/.local/share/opencode` (including `auth.json`, which holds the login) and
  `~/.cache/opencode` are readable, as are `~/.config/opencode` and `~/.opencode` (the install
  script's home, which v2's config discovery stats), because OpenCode needs them, so credentials are not
  denied to the model at the OS layer there; the config's `external_directory: deny` keeps the
  model's tools to the project. `~/.zshrc`, `~/.ssh` and the rest of HOME stay unreadable (checked
  with `sandbox-exec`). Both read-only and writer profiles allow literal `file-read-metadata`
  on `.claude` and `.codex` directories along the cwd ancestry after protected-path denies.
  These grants permit discovery's `lstat` without granting content or descendant reads, and
  do not follow a leaf symlink. The existing Claude-disable switches suppress instruction
  loading; they do not establish that v2.0.32 skips these metadata probes. Sandboxed lanes
  disable the `fabric` MCP server through the [v2 per-server switch](https://opencode.ai/v2/docs/mcp-servers):
  `mcp.servers.fabric = {type: "local", command: ["provenant"], disabled: true}` in the inline
  config. V2 replaces the whole server definition, so the disabled entry includes its required
  type and command; other servers keep their configuration. The run also sets `OPENCODE_DISABLE_CLAUDE_CODE`, `..._CLAUDE_CODE_SKILLS`,
  `..._AUTOUPDATE` and `..._FILEWATCHER` so foreign instruction files, auto-update and watchers stay out.
- **Herdr plugin.** A user plugin `~/.config/opencode/plugins/herdr-agent-state.js` fails to load
  in v2 ("must export default definition with id and effect/setup"). It is logged as a warning
  and is non-fatal; it lives outside the repo and needs updating by its owner.

## Cursor, Kiro, Copilot

- These are backup lanes for breadth or when a primary is rate-limited. Kiro is
  a cheap gateway to other families; Copilot is a multi-model gateway with
  per-call overhead like Codex. Run Cursor on its own native model.

## Test environments

- A suite that passes locally can depend on developer `HOME` state. When CI
  fails and local passes, vary one axis at a time (real or empty `HOME`, real
  or reduced `PATH`) and prove which moves the result. Build a reduced `PATH` by
  subtraction (everything except the named binaries), not from a short allowlist.
- Do not run `uv sync` in a linked worktree as a reflex. Where the interpreter
  resolver borrows the primary checkout's virtualenv, a half-built worktree
  `.venv` shadows it and manufactures environmental failures. Compare the
  failure count on the integration branch, in the worktree with the diff and
  without it. A typed `interpreter_unavailable` status is the resolver reporting,
  not the code failing. Remove the stray `.venv` to resume the borrow.
