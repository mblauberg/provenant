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
  Stderr carries INFO logs even at `--log-level error`; only `level=ERROR` lines are fatal.
- **Read-only sandbox.** v2 resolves every ancestor of the cwd, `~/.claude` and `~/.agents` and
  follows `@imports` in instruction files at startup, so a blanket home read deny makes the
  server fail with `UnexpectedStatus: 500` (EPERM). For OpenCode the macOS profile denies the
  credential stores (`CREDENTIAL_STORES`, which include `~/.claude` and `~/.codex`), the rest of the
  workspace root and `/private/tmp`, and allows listing (not reading) of the cwd ancestors.
  The config's `external_directory: deny` keeps the model's own tools inside the project. The
  run also sets `OPENCODE_DISABLE_CLAUDE_CODE`, `..._CLAUDE_CODE_SKILLS`, `..._AUTOUPDATE`
  and `..._FILEWATCHER` so foreign instruction files, auto-update and watchers stay out.
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
