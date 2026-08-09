# Codex subagents (Codex-only layer)

Verified against OpenAI Codex/GPT-5.6 docs on 2026-07-10:

- `https://learn.chatgpt.com/docs/agent-configuration/subagents`
- `https://developers.openai.com/api/docs/guides/latest-model`
- `https://developers.openai.com/api/docs/guides/tools-multi-agent`

Codex subagents are native parallel workers inside a Codex session. For eligible
models/accounts, GPT-5.6 `ultra` uses maximum reasoning and proactively delegates
suitable work to subagents. At other effort levels, make fan-out concrete in the
prompt. Ultra/native multi-agent is a first-class workflow substrate, but Codex
does not execute Claude Code dynamic workflow JavaScript unchanged: keep the
state machine portable and realise it through collaboration actions, run-dir
files and root-thread reduction.

```
Spawn one subagent per source slice. Wait for all results. Summarise by slice with file paths and
unresolved questions.
```

## Built-in roles

- `explorer`: read-heavy scouting, source discovery, repo questions.
- `worker`: bounded implementation or artefact work with an owned write scope.
- `default`: general sidecar work when no narrower role fits.

Custom Codex agents are project-scoped configuration files under `.codex/agents/*.toml` when the
project uses them. Do not assume a custom agent exists; discover current tools at runtime.

## Routing rules

- Prefer a GPT-5.6 flagship Codex lead at `ultra` for substantial-to-terminal
  orchestration when the runtime exposes it. Record a fallback to `max`,
  `xhigh` or `high`; do not assume the entitlement exists.
- Use Codex native subagents for same-harness fan-out. Do not use `codex exec` as a substitute for
  Codex subagents inside Codex.
- Use `codex exec -s read-only --ephemeral` as a noninteractive verifier only when the orchestrator is
  another family.
- Under `-s read-only`, a synthesis written via `apply_patch` is rejected. Analysis and report
  workers use `-o <path>` to persist the final message directly outside the sandbox. A
  `codex exec` run that outlives its foreground window is recovered by waiting on the detached
  PID (`worker-liveness.md`), never by relaunching beside it.
- For many slices, dispatch native subagents in adaptive waves and keep full outputs in run-dir files.
  After each reduce step, decide whether to widen, narrow, repair, verify, document, or stop.
- For orchestrated work, run cross-family reviewers alongside native subagents when data policy and
  tooling allow it. Native subagents usually cover narrow sections; cross-family workers usually cover
  broader architecture, omission, contradiction, or adversarial lenses. Record certified/advisory/not-run
  status in `crossfamily/` and `traces/`.
- If the current harness exposes a workflow/thread primitive, record the actual primitive used in
  `traces/`.
- Native Codex collaboration status and Herdr pane status are separate control
  planes. Never merge them into one agent state. If they conflict, inspect the
  target through its owning control plane and live execution evidence; for
  Herdr use `herdr agent explain <target> --json` plus a bounded pane read.

## Prompt shape

Good Codex worker prompt fields:

```
role:              explorer | worker | default
task-class:        mechanical | legwork | critical-review | orchestration
tier:              scout | workhorse | flagship
catalog-model:     Luna | Terra | Sol
effort:            <effective effort from route receipt>
route-receipt:     <path or receipt identity>
scope:             <files / sources / task slice>
write-scope:       read-only | exact owned paths
governing-skill:   <skill owning the change; required for write workers>
must-not:          <shared writes, destructive commands, stale sources>
output-path:       <run-dir>/findings/<name>.md
return:            3-6 bullets, surprises, unresolved, file path
```

For subscription-native Codex workers, omit the literal transport `model` and bind the resolved `effort`.
Retain Luna/Terra/Sol as the catalogue identity in the
receipt. If the native surface cannot bind that effort, stop or use an
authorised adapter and record the substitution. Do not silently inherit the
chair route. Explicit chair inheritance is valid only when the dispatch and
receipt say so.

Codex should consolidate results after all requested subagents finish. The orchestrator still decides
which findings are supported; do not majority-vote weak claims into truth.

## Portable workflow execution

This is the Codex/Cursor adapter to the substrate-neutral stage/gate/recovery
graph in `orchestration-contract.md`: express a workflow as phases, receipts,
gates and recovery transitions first. Claude may bind that graph to
`Workflow()` JavaScript (`dynamic-workflows.md`). Codex Ultra/native
multi-agent may choose and coordinate subagents adaptively; explicit waves keep
the same graph available at lower efforts:

- **Small feature**: scout -> implement -> section review/docs -> verify.
- **Large task**: orientation -> wide scout -> scoped section waves -> repair -> cross-family broad
  review -> more section waves as needed -> document update -> verification -> closure.

Use a driver script only when repeatability, resume state, or a large slice list justifies the extra
surface. Otherwise, native subagents plus run-dir files are the Codex-native path.

**User gate mechanics.** Neither native subagent collaboration nor an explicit-wave script can pause a
live Codex run mid-collaboration and block on user approval. Realise the contract's user gate by
ending the run at the gate-adjacent stage and recording `awaiting-user` in the run-dir manifest/receipt;
a user-approved follow-up invocation continues the graph. It is a new run reading the prior run-dir
state, not an in-process resume of a suspended session.

GPT-5.6 Programmatic Tool Calling is a separate Responses API substrate for
bounded, tool-heavy stages with predictable data flow. It can run generated
JavaScript in OpenAI's hosted runtime, but it is not Claude workflow JavaScript
and should not absorb semantic review, approval-sensitive writes or user gates.
