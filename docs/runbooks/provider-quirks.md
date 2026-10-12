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
- Do not route a family's models through another vendor's tool (for example
  GPT or Claude through a Cursor seat): limits burn fast and attribution blurs.
- Prefer a native subagent for your own family and a headless CLI for another.
  Never self-deploy through your own headless CLI.
- Strip ANSI noise from CLIs that emit it before parsing their output.

## agy (Antigravity, Gemini family)

- Headless `-p` cannot prompt for tool permission. A denied call exits 0 with
  `{"status":"SUCCESS","response":""}` and the only real signal on stderr. One
  denial discards the whole turn, reads included. Judge the dispatcher's record
  status, never exit code or output size, and do not merge stderr away.
- `--add-dir` grants reads. Wildcard permission rules work for some categories
  (`mcp`, `command`) but not `read_file`/`write_file` paths.
- `--model` and `--effort` are separate flags; `--effort` is mandatory and
  efforts are per model (some offer only low and high). There is no `-m` alias.
  An unknown slug can silently fall back to a cheaper model: confirm the
  reported model. Evidence from before the release that fixed `-p` ignoring
  `--model`/`--effort` is void.
- The prompt is one argv value. `--print -` treats the dash as the literal
  prompt. Linux caps one argv string at 128 KiB, macOS does not, so a large
  brief works on a Mac and fails on a Linux runner: keep briefs under about
  124 KiB and put bulk material behind `--add-dir`.
- Large or code-heavy prompts have hung with zero output, and `--print-timeout`
  is not honoured. Send compact per-region prompts and wrap the call in an
  external watchdog. Treat agy as advisory.
- Quota errors are account-wide and name the reset window: read it rather than
  retrying. Probe quota with the model the real lane will use.

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

## OpenCode, Cursor, Kiro, Copilot

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
