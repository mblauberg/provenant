# Workflow Rust design

## Decision and boundary

Replace the workflow machinery with one small synchronous policy library, a
minimal delivery receipt and a bounded Markdown session checkpoint. Keep a
pure review coverage check; for PR work the PR carries the evidence. Cut workflow
engines, generated reference runs, duplicated queues and the entire bespoke UI
runtime. Skills conduct the work using `git`, `gh`, Fabric and browser tools.

This is a design for both implementation chairs, not an implementation or
cut-over approval. The binding [cross-area decisions](https://github.com/mblauberg/provenant/blob/d61c4335a5c3e9ef3c5761b21fc710354226723e/docs/specs/rust-port/design/README.md#cross-area-decisions)
and [PORTING v7](https://github.com/mblauberg/provenant/blob/d61c4335a5c3e9ef3c5761b21fc710354226723e/docs/specs/rust-port/PORTING.md)
govern this design; the table wins any conflict, including older R1 import
wording. V7 supersedes the transition, compatibility and parity sections of
[ADR 0028](../../../adr/0028-rust-port-dependencies.md) and the [port plan](../README.md).
Their dependency allowlist, typed errors, serde records, crate layering,
process custody and sole unsafe boundary in `supervisor::sys` still apply.
ADR 0028's small stderr diagnostic module governs the conflicting R8 reference
to `tracing`. No new external Rust dependency is proposed.

Only provider CLI wire forms, harness hook protocols, Agent Skills `SKILL.md`,
MCP and git survive as external contracts. Our verbs, tool names, schemas,
configuration and state paths are redesigned. There are no selectors, adapters,
Python/Node test oracles or old-implementation parity gates. Main may be red
for retired paths. Each Rust owner removes its old source and obsolete tests;
installation changes once, after the complete Rust product is ready.

Apply the agreed v7 rule, “The stable install during the rewrite”, in the
[port README](../README.md) and [PORTING R11](../PORTING.md): the primary checkout
on each Mac is the frozen live install. Never pull, check out another revision
or reinstall it until cut-over. All work uses worktrees from `origin/main`.
Deletions may merge to main freely while that checkout stays frozen. Do not
re-point symlinks or edit `~/.claude/settings.json` during the rewrite.
PORTING R11 on the binding ref publishes this frozen-install rule; the local
port-plan baseline predates it. Final cut-over follows the table's installer
contract, including authorised global instruction updates.

Evidence baseline: checkout `26521609fc114e85946aa5d43ef67828b8e2490f`;
`docs/rust-port-slices` at `4201ef9f9185ae4fed23ff5d0e03d15e13561c40`.
SLICES cards 03, 09, 10, 25 to 27, 30, 39 to 47 and 49 are an inventory,
not implementation dependencies. Physical line counts below include comments,
blank lines, generated bundles and vendor copies where tracked.

## Users and observed demand

| User | Need | Current evidence and resulting boundary |
|---|---|---|
| Agents following skills | Record evidence and determine what prevents acceptance | `skills/deliver/SKILL.md:31,63` calls producer and validator; `skills/implement/SKILL.md:74` validates software delivery. Retain typed recording and checking. |
| Chair conducting review | Count independent reviewers with adequate tiers, lenses and family coverage | `_shared/review_plan.py:57` validates topology; `orchestrate/scripts/run_dir_finalize.py:395` consumes it. Consolidate topology, terminal and delivery review checks. |
| User at CLI | Inspect a receipt, understand a refusal, save/resume a checkpoint | `session/references/chair-state.md:64,98` and `context-hygiene.md:59` document direct helpers. Provide short verbs and actionable diagnostics. |
| Machine callers | Obtain structured results | CLI `--json` suffices; `runtime/fabric/src/server.ts` exposes no workflow demand. Add no workflow MCP tools or crate edge. |
| Harness hooks | Restore the correct session after compaction | `session/scripts/state_check.py:84` emits PreCompact and SessionStart hook configuration. Keep the host's JSON stdin/stdout contract; replace transcript searching with an explicit checkpoint key. |
| Long-running skill | Resume a goal and distinguish idle waiting from completion | `autopilot/SKILL.md:32`, templates/README:68 and `validate_idle_pause.py` use a second Markdown queue. Cut the idle validator; skill guidance reads top-level `lanes` and Fabric tasks. |
| Writing skills | Locate mechanical prose defects, including legal/academic rules | `legal-writing/SKILL.md:73`, `natural-writing/references/academic-prose.md:310`. Retain one matcher with three rule sets. |
| Repository setup and tracker | Follow the declared tracker and avoid duplicate issue work | `setup-repo/SKILL.md:76` offers the shell-detecting hook; `tracker/references/steward.md:29` offers replay bookkeeping. Replace with skill instructions and existing tasks plus `gh` reads. |
| Installed workflow host | Discover `meta` and execute `run` JavaScript exports | `scripts/install-workflows:46` globs sources; `scripts/install-harness:366,556` installs them. Claude Code owns this external Workflow host contract. Retire our accelerators, which hard-code deleted scripts; no Provenant Node or Rust host replaces it. |
| Other crates and hosts | Link a delivery to attempts and report its outcome | `runtime/fabric/src/execution-input.ts` carries `delivery_run_id`; `scripts/telemetry_delivery.py:37` ingests receipts. Replace this with typed `DeliveryId` links in run records; the receipt plus run records are the record. |

Paths beginning with a skill name in this document are relative to `skills/`.
Evidence of a call in a skill is evidence of a supported path, not a measured
invocation count. State sampling is read-only, bounded and excludes prompt,
message and credential contents.

At the evidence baseline, the legacy telemetry ledger at
`~/.local/state/agent-harness/fabric/telemetry.sqlite3` exists: about 565 MB,
566,185 usage rows, 566,877 usage-file rows, 111,191 observations, 46,923 events,
13 deliveries and zero changes at the baseline inspection. A repair-pass
read-only aggregate query confirms **13 deliveries, none accepted**, no
`merge_commit` and no `acceptance_basis` (`COUNT`, `SUM(accepted)` and non-null
counts on `deliveries`). Helper invocation frequency is not established by
those rows; transcript regex samples are indicative only.
No telemetry database was found under `~/.local/state/provenant`.

## Feature verdicts

`*` marks a user-visible removal or intentional change. Policy text and caller
instructions change with their owner; an obsolete script path is not retained
as a compatibility wrapper.

| Feature | Verdict | Reason and replacement |
|---|---|---|
| Risk derivation and explicit downgrade evidence | Keep | Derive the minimum from typed factors; lowering it still requires user authority. |
| Authority mapping and delegation containment | Merge | `confinement` owns grants; workflow receives the confined actor context and records required approval references. |
| Review ladder, plan, terminal and delivery review validators | Merge | One typed review model checks actual attempt, verdict, subject and independence once. |
| Panel presets, axis overrides, membership and reducer registry | Cut* | Skills can request lenses and synthesise findings; panel metadata adds no assurance beyond individual reviews. |
| Provider-family maps in workflow | Cut | Providers/routing own verified lineage; workflow must never guess a family from a name. |
| Portable-workflow context stamper and per-skill wrapper schemas | Cut* | A completed context stamp proves no outcome; use the PR or the skill's document for evidence. |
| Flat approval predicates and closed-run definitions | Merge | A minimal recorded decision replaces string walks; evaluate and release own their checks. |
| Five delivery profiles, overlays and policy callback dictionaries | Cut* | Skills state criteria directly; delete the profile config, schema and scenario suite. |
| Exact subject and retained references | Keep | Bind reviews to a PR head or document digest; run and confinement own custody. |
| Evaluation bindings and security evidence | Cut* | Evaluate, release and domain skills own their evidence and obligations. Keep those references in their owning skill documents or PR, outside the minimal receipt. |
| Outcome and trajectory measure framework | Cut* | Keep scoped criteria in the skill document or PR; remove universal metric records. |
| Receipt mutation commands and repair/finding sidecars | Simplify* | Replace a bounded receipt at an expected revision; findings and repairs belong in PRs or the skill document. |
| Evidence subprocess runner in delivery | Cut* | Execute through `run` or an authorised ordinary command, retain its result in the PR or skill document. No second custody owner. |
| Reference run/evaluation builders and security selector | Cut* | Rust test constructors cover the new design; synthetic passing records are not a product feature. |
| Merged delivery binder and implementation checkpoints | Cut* | The repository workflow owns exact-head CI and merge; session Markdown carries resumable progress. |
| Session Markdown shape checks, audit and transcript reinjection | Simplify* | Save bounded Markdown under the host session ID; SessionStart injects that key and the save command, with missing/stale status. |
| Scratch cleanup from session helper | Cut* | Retention belongs to the shared cleanup owner; no workflow deletion command or old-root traversal. |
| Mission scaffolder, knobs parser and Markdown item leases | Cut* | Skill templates remain optional documents; Fabric tasks and run admission own work and leases. |
| Idle-frontier pause validator | Cut* | Autopilot reads lanes/tasks directly; run owns admission and actual pause. No workflow frontier API. |
| Tracker replay queue and outcome writer | Cut* | A file write cannot make a remote GitHub side effect exactly once; reconcile issues/tasks before retry. |
| Tracker Bash command parser and denying hook | Cut* | Shell recognition is incomplete; repository tracker instructions and command wrappers own routing. |
| Herdr CLI markdown/help contract checker | Cut* | Workflow no longer invokes Herdr; the external integration owner tests any retained transport itself. |
| JS implement-run, cross-verify and codebase-polish engines | Cut* | Existing skills plus Fabric already conduct these sequences; no replacement generic workflow interpreter. |
| Shared root, exclude, config-loading, bounded process and custody helpers | Merge | Core/config/confinement/supervisor own these services; no workflow copies. |
| Writing, academic and legal linters | Merge* | One small matcher behind `provenant check writing PATHS` (decisions table, Checks and hooks). Rules are JSON beside the owning SKILL.md, outside policy/config/records. |
| Delivery telemetry, cost per accepted outcome and retrospective inputs | Simplify | No ledger or summary ingestion. `telemetry stats` reduces terminal run records with prices; receipts retain decisions and run records carry `DeliveryId`. Preserve missingness. |
| UI detector, live server and browser overlays | Cut/replace* | Component decisions below remove all UI runtime ownership from workflow. |

## UI component decisions

Retain the `ui-ux-design` judgement skill. Replace browser operations with the
existing optional `library/playwright/SKILL.md` workflow and the target app's
own dev server. The external tool supplies navigation, accessible snapshots,
actions and screenshots, not design conclusions. It is not a dependency of the
Rust binary; check availability/version and never download it implicitly.

| Component | Evidence of use | Recommendation and deliberate loss |
|---|---|---|
| Text scanner, finding/severity model, registry and rules | `ui-ux-design/references/review.md:24` and `visual-qa.md:24` call the detector as supporting evidence | Delete. Keep source/render review; remove heuristic anti-pattern scores and detector JSON output. |
| Static HTML and CSS cascade/colour engine | `runtime/ui-evidence/detector/cli/main.mjs` selects static/file modes | Replace rendered facts with browser evidence; delete the partial HTML/CSS renderer and static detector mode. |
| URL browser launcher and DOM rule injection | `detector/engines/browser/detect-url.mjs` is called by detector orchestration | Replace navigation/inspection with Playwright; delete automated heuristic DOM findings. |
| Screenshot contrast and profiler | Detector browser/visual engines call these helpers | Replace capture with Playwright; delete custom contrast heuristics and detector profiling. |
| Detector CLI, path resolvers and wrappers | Two documented detector calls above; `skills/ui-ux-design/scripts/detect.mjs` resolves the runtime | Delete the commands and resolvers; rewrite both skill references. |
| Browser bundle source and build/test machinery | `runtime/ui-live/live-server.mjs:52,365` loads/serves the generated detector bundle | Delete together with both consumers; it is a live asset. |
| Source containment, JSX scanner, wrapping and generated-file guard | `runtime/ui-live/live.mjs`, `live-wrap.mjs`, `live-inject.mjs` consume them | Delete UI-specific source transformation. Ordinary edits use implement and generic confinement. |
| Context/design parsers and path resolution | `load-context.mjs` and `design-parser.mjs` feed custom live setup | Delete; skills read project context without another parser/registry. |
| CSP inspection and source injection | `ui-ux-design/references/live.md:22` describes setup/injection | Delete CSP rewriting and script injection; use app-owned preview. |
| Event journal, lease and resume/status/completion helpers | `references/live.md:22` names their entry points; server owns the session store | Delete; no new UI state and no old-journal import. |
| Loopback HTTP/session server and startup/process owner | Live setup launches `runtime/ui-live/live-server.mjs`; harness checks it at `scripts/check-harness:179` | Delete server, endpoints, long polling and process wrappers; the app owns preview/hot reload. |
| Browser session client and injected panel | Server serves `live-browser.js` and `live-browser-session.js` | Delete custom overlay and in-page session UI. |
| Comments, strokes, annotations, variants and accept/discard | `references/live.md:22` documents the custom interaction; `live-accept.mjs` applies variants | Delete these product features. Ordinary UI implementation plus browser checks does not reproduce their interaction. |
| Vendored modern-screenshot bundle/licence | Server serves it at `live-server.mjs:744` for annotation capture | Delete both; screenshot capture comes from the external browser tool. |
| Runtime provenance manifests and generated/vendor notices | Runtime YAML ledgers and `tests/test_ui_ux_provenance_contract.py` assert them | Delete runtime ledgers and screenshot notice; preserve Impeccable provenance/licence for retained skill prose. |

No component warrants a Rust port. Saved run command events contained eight
mentions of UI paths, all source inventory/searches and zero direct UI script
invocations. No UI/telemetry-named artefacts were found in the sampled
`~/.local/state/provenant` or `~/.cache/provenant` trees. This is limited
negative evidence, not proof of no historical use. Documented detector/live
calls and internal bundle/vendor consumers above are positive use evidence.
The three accelerators are present in `~/.claude/workflows`; retiring them
removes `/implement-run`, `/cross-verify` and `/codebase-polish`. They execute
in Claude Code's Workflow host, an external contract. Our scripts hard-code
`delivery_receipt.py`, `validate_delivery.py` and `cf_dispatch.py`
(`workflows/implement-run.js:395,406,461,780,802`); the host is not a Provenant
Node runtime. Source listings do not establish accelerator execution.

`library/playwright/SKILL.md:8,36` already documents the replacement browser
operations and its eval cases compose it with UI review/implementation. The
[Microsoft CLI documentation](https://github.com/microsoft/playwright-cli#commands)
also describes navigation, snapshots, actions and screenshots. It requires
Node in the optional external operator tool, not in Provenant's install.
Keep the UI skill's read-only review boundary. Where no authorised browser is
available, report the browser evidence as untested. Add no Rust DOM/parser,
HTTP/browser server, generated UI bundle or UI-specific MCP tool.

## Rust owner and core model

Keep `tools/front-door/crates/workflow`, with `#![forbid(unsafe_code)]`.
Allowed dependencies remain core, records, config and confinement. External
crates remain serde, serde_json, thiserror, regex and sha2. No SQLite, process
runner, scheduler, MCP tool or new external dependency belongs to workflow.

| Module | Responsibility | Production lines, including edge adapters |
|---|---|---:|
| `review` | Pure independence, tier/lens/family coverage and exact subject check | 400 |
| `receipt` | Minimal subject/authors/risk/review references/decision | 200 |
| `store` | Bounded read, expected revision and atomic replacement | 150 |
| `session` | Bounded Markdown save/read and hook context | 150 |
| `writing` | Span exclusion and data-driven matching | 300 |
| Public API/errors | Typed requests and deterministic diagnostics | 100 |
| CLI/hook edge, outside the crate | Parse, gather facts and render | 150 |
| **Target** | Shared owner implementations excluded | **1,450** |

Records owns the receipt and review wire types, provenance and risk/review
policy records. Workflow owns their semantic checks. Markdown is the session
payload, with no typed JSON checkpoint schema. Writing rules belong beside
`skills/natural-writing/SKILL.md` and `skills/legal-writing/SKILL.md` as
`lint-rules.json`; general and academic rule sets share natural-writing's data.
No writing rules enter config or records. Risk/review settings and bounds come
from typed values loaded from the single user-owned `config.toml` under the
state root, with compiled defaults. There is no profile layering, project
overlay, per-knob environment input or profile mutation command; `config show`
and `config check` remain, with an optional `profile =` selector. Product env
inputs are only `PROVENANT_STATE_DIR` and `PROVENANT_AGENT_LABEL`; provider and
harness variables pass through as their tools define them.

| Type | Minimal contents |
|---|---|
| `Subject` | `Pr { repository: RepoId, number, head: GitOid }` or `Document { path: RelPath, digest: Digest }`. Reviews bind this exact subject. |
| `DeliveryReceipt` | Version, delivery/project IDs, revision, subject, authors, risk, review attempt references, decision, created/updated times. |
| `Author` | `ActorId`, user/agent kind and route provenance including actual tier for an agent. |
| `Decision` | Pending, accepted or abandoned; actor, time, reason and any required originating user approval reference. No release or close states. |
| `ReviewFact` | Attempt/reviewer IDs, subject, output reference/digest, verdict, blocking findings, route provenance and named lens. |
| `ReviewContext` | Authors, decision-maker IDs, risk, current time, policy and routing availability facts. |
| `ValidationReport` | Sorted code/field/message/remedy diagnostics, missing review legs and status. |

For PR work, the PR is the evidence: exact-head required CI, review findings
and repairs, then merge through the repository's workflow. A receipt records
the chair's decision; it is not a second merge certificate. Cut `MergeFact`
tree-relation validation, requirement/evidence registries, evaluation and
observation rules, release/close transitions and universal outcome metrics.
Evaluate, release and domain skills retain their own obligations. For document
work, record its digest and the existing evidence location, without a new blob
store. A changed subject requires fresh review and a new pending decision. IDs and
creation time are immutable; each save rechecks derived risk and requires
originating approval for a downgrade.
Authority narrowing remains confinement's job; risk downgrade and terminal or
scope-required acceptance retain their explicit user approval gates.

Review passes only for a retained matching verdict/output on the exact subject,
with adequate tier and independent reviewer identity. Authors and decision
makers, including the chair, cannot count as reviewers. Repeating a reviewer
adds no coverage. `approve_with_nits` passes only if all remaining findings are
advisory. Failed work or missing verdicts are not provider unavailability.

| Provenance | Author tier | Same-family review | Cross-family review |
|---|---|---|---|
| `Observed` | Counts when the route/tier is verified from the attempt or host record | Counts | Counts only with providers-verified model-family lineage |
| `ChairAttested` | Counts; chair records the native host model parameter, tier and evidence reference | Counts, with independent actor and lens | Never counts |
| `Unknown` | Blocks substantial-or-higher coverage | Findings only | Never counts |

Chair attestation does not promote itself to observation or verified family
separation. An attested native author supplies tier and declared family for
policy inputs; a cross-family review leg still needs an observed, verified
reviewer route separated from every declared/verified author family. If an
author's family is unknown, separation is unproven. Keep adapter, endpoint
provider, model family and review eligibility as separate fields; never derive
family from model spelling. Native hosts retain the originating launch/model
parameter, not an agent's self-report.

| Risk | Counted independent passing reviews | Coverage |
|---|---:|---|
| Routine | 0 | Chair plus objective checks. |
| Substantial | 1 | Other-family if available; otherwise a recorded blocker and same-family reviewer through a distinct lens. |
| Crucial | 2 | Distinct lenses and two other families if available; each unavailable leg has a recorded blocker and independent same-family replacement. |
| Terminal | 2 | Crucial coverage and an adversarial lens, which may be one of those two counted reviews; explicit user acceptance. |

Every counted reviewer meets the highest author tier. Routing availability
includes quota/config/capability reasons and family-slot shortfalls with source
references and expiry; a reason string alone cannot waive a leg. Distinct
lenses and the required independent count survive fallback. Measure family
separation against all authors, not the chair's family. Cover a chair whose
family differs from the author's in native tests.

Fact types implement `Serialize` but not `Deserialize`; only composition-edge
code builds them from owner reads. This prevents request JSON being decoded
as verified facts. It is a code boundary, not an unforgeable/private-constructor
guarantee across crates. Workflow receives facts as values and never depends
on run, routing or providers.

## Persistence, concurrency and errors

Use the host-global `~/.local/share/provenant/v1` root. `--state-dir ABS`
overrides `PROVENANT_STATE_DIR`, then this default. No old-root fallback; refuse
a new root inside an old root. Runs and their store stay at the top level,
Fabric under `fabric/`, and installation under `install/` and `releases/`.
Workflow proposes `workflow/projects/<project-id>/` beneath that one root,
using host-local `ProjectId` for receipts and checkpoints, not separate roots
per project. The Open decisions row requests this subtree in the binding table:

```text
deliveries/<delivery-id>/receipt.json
deliveries/<delivery-id>/receipt.lock
sessions/<host-session-id>/checkpoint.md
sessions/<host-session-id>/checkpoint.lock
```

Confinement supplies held directory handles and validated ID components.
No old-root writes, directory scan to guess a session, lossy path conversion
or `..` components. Git OIDs support SHA-1 and SHA-256. Records performs
bounded decode and canonical JSON encoding: sorted object keys, UTF-8, integer
UTC microseconds and no final LF in canonical bytes. Files written to disk may append LF; digests cover the canonical bytes without it. Reject duplicate keys, unknown versions/tags,
oversized input and invalid integers. Limits: receipt 64 KiB, 256 review refs,
checkpoint 32 KiB and hook output 8 KiB. Reports have deterministic ordering.

Save the entire receipt at an expected revision. Acquire a stable lock inode,
read/check revision, validate the replacement, write/sync a same-directory
temporary file, replace the receipt and sync the directory. Increment revision
once. Do not unlink/recreate locks. No blob store, event log, operation-ID
history or `CommitUncertain` protocol. On I/O failure after replacement, report
the failed sync and tell the caller to reread; do not claim rollback. Retry
starts from the observed revision and decision, rather than replaying an
operation batch. A crash leaves an old or new complete file; durability still
requires the successful directory sync.

The supervisor deadline method is `FileHandle::lock(LockOptions { mode:
Exclusive, timeout: Some(remaining) })`: it polls non-blocking `flock` on a
monotonic deadline (`supervisor/src/fs/path_io.rs:43`). Confinement must expose
that bound on the stable, no-follow-opened lock handle; the path-following
`open_append` helper is not a safe store opener. An extension for cancellation
checks belongs to supervisor. Never hold a receipt lock across routing probes,
provider waits or GitHub reads. Gather facts first, then recheck subject and
expiry under the lock.

Core supplies production `CommitClock::now() -> UtcMicros` through `jiff`;
tests inject a clock. Read time after acquiring the lock. Set `updated_at` to
`max(previous.updated_at, now)` and diagnose a clock regression; use actual
`now` for expiry checks. Pure review policy reads no process-global clock.
Errors include malformed/version/limit, stale subject, authority denied,
conflict, contention, cancellation, deadline and I/O, with redacted path/cause.
Cancellation before publication has no committed effect; once replacement
starts, finish the bounded local write and report its result. Workflow starts
no threads or subprocesses and never cancels another owner's attempt/lease.

## CLI, writing and hook surface

The [CLI design in PR #1359](https://github.com/mblauberg/provenant/pull/1359)
owns the command envelope and verb tree. Its `workflow` subtree is reserved;
the following leaves are proposed. Writing checks use the shared `check` verb
(decisions table, Checks and hooks): `check writing PATHS` runs workflow's
writing rules as data and exits 1 on findings.

```text
provenant workflow review --request <file|-> [--json]
provenant workflow receipt show <id> [--json]
provenant workflow receipt save <id> --expected-revision <n> --request <file|->
provenant workflow session save <session-id> --file <markdown|->
provenant workflow session show <session-id> [--json]
provenant hook claude pre-compact|compact
provenant check writing <paths...> [--rules general|academic|legal] [--json]
```

`review` is the pure coverage check. A new receipt uses expected revision zero;
`save` replaces its minimal fields with trusted actor context supplied by the
host, never by JSON claiming authority. `show` reports stored claims, without
claiming current verification. CLI renders one UTF-8 JSON object plus LF:
`{ "schema_version": 1, "command": "workflow.review", "status": "ok", "data": {}, "error": null }`.
Status is `ok`, `failed`, `refused`, `cancelled`, `timeout` or `error`; errors
carry a stable code, redacted message, cause and optional repair. Useful partial
coverage and input-required state go in `data`, never an exit 4. The whole
result is capped at **1 MiB**; paginated lists carry `next_cursor`, and tails
are at most 500 lines/64 KiB. Never silently discard rows. Text reports short
status and remedies.

The binding exits are 0 successful operation/query, 1 failed/refused/findings,
2 invalid input/version, 3 cannot proceed (setup, I/O, conflict/contention or
unavailable required evidence), 124 wait deadline and 130 cancelled. A wait
whose condition is met, including `InputRequired`, exits 0. Waits require an
explicit budget; CLI permits up to 14,400 seconds. The binary edge renders
these outcomes; libraries never choose exits or return an envelope.
No workflow MCP tools or `fabric-mcp -> workflow` edge: CLI `--json` only.

The matcher loads bounded JSON rules from the selected skill, compiles Rust
regex/literal rules once and excludes code fences, inline code and declared
LaTeX command/math spans. Reject unsupported patterns and oversized rule sets;
report deterministic rule IDs, Unicode scalar line/column and byte ranges.
Legal checklist failures remain filing blockers in that skill. Low sampled
invocation counts do not justify removing a live legal requirement.

Checkpoint content is ordinary Markdown: goal, authority/evidence references,
progress, next actions, blockers and resume triggers. Bound bytes, require
UTF-8 and use the explicit host/project key, without a shape validator or JSON
wrapper. `save` writes atomically under that session's lock. Save updates must
preserve originating authority references; prose cannot expand a grant.

Run owns the top-level `dispatch`, `lanes [--wait]`, `output`, `cancel`,
`resume` and `reconcile` verbs. `reconcile` recovers queued/orphaned work after
scheduler death. Fabric keeps messaging, tasks, agent labels, peer hosts and
MCP; named sessions retire in favour of run `resume`. The table overrides
CLI's older `fabric.lanes` examples; CLI freezes exact run flags. Workflow
adds no execution verbs and never calls old orchestrate scripts.

Hooks are rows in cli's `HookSpec` table, invoked as `hook claude pre-compact`
and `hook claude compact`, with host-native stdout/exit contracts rather than the CLI envelope. They read
at most 1 MiB host JSON stdin and emit at most 8 KiB context. Advisory callbacks
exit 0 even for missing/invalid state, with a bounded diagnostic. SessionStart
obtains the actual session ID from the host event and injects the validated key
and this save command with the actual ID substituted:
`provenant workflow session save <session-id> --file <checkpoint.md>`.
It then injects only that key's bounded checkpoint, or an explicit missing/stale
status. PreCompact reports whether that checkpoint needs saving; it does not
search transcripts. Missing/invalid host ID produces an explicit diagnostic
and no checkpoint, never a fallback to the latest file. Keep native fixtures
for host field names and omission/error behaviour. Cut the idle-pause validator and `work_frontier` ask;
autopilot guidance reads lanes/tasks directly and run owns pause/admission.
Config retires the tracker hook in the final install while preserving unrelated
and user-owned config.

## State reset and useful reuse

Fresh state is mandatory. Leave old `~/.agents`, `.agent-run`, transcripts,
telemetry and installed content untouched during the rewrite. Never import old
RUN.json, session Markdown, mission queues, tracker replay files or UI journals.
Active work resumes by an agent reading old documents and explicitly creating
a new authorised receipt/checkpoint, not by a hidden migration command.

The table narrows PORTING R1: **Fabric alone owns the read-only, one-shot
cut-over import**, covering open and blocked tasks plus unacknowledged requests
from the last 14 days. Point agents with active old tasks to that import, not
manual recreation. The earlier sample of 25 open/five blocked tasks describes
the old store, not a separate workflow migration or a frozen import count.
Fabric owns project/identity mapping; imported text grants no new authority.

There is **no telemetry ledger** in the new product. `telemetry stats` reduces
terminal run records with the bundled price table. The receipt plus run records
are the record; workflow publishes no delivery summaries and requests no
telemetry write/report API. The old ledger stays on disk for the old install,
neither imported nor deleted, and its history is absent from new reports.
The OTel collector retires at cut-over. No workflow import is added.

| Material inspected | Keep | Strip or move |
|---|---|---|
| Current workflow Rust, 8,771 src lines plus 371 test/fixture lines | Review coverage cases, span/exclusion logic, authority containment cases, bounded reverse-reading lessons and explicit custody refusals | Rebuild around typed models; remove `pending`, Python whitespace/word/dictionary encoders, oracle harness and old CLI parser/rendering. Native cases survive by policy meaning, not old byte output. |
| [PR #1344](https://github.com/mblauberg/provenant/pull/1344), head `8ba2fd9665bef40d433526fdf667b727d3851ce4` | `delivery_run_invariants` risk/closed-write cases; receipt operation preconditions; `context_audit` path escape/freshness cases; style spans and bounded host traits | Do not merge wholesale: cut Herdr checker, tracker shell lexer/queue, bootstrap template engine, reference evaluator, legacy receipt commands and all Python oracles. Move `lane_launch`, `lane_exit`, `provider_observer`, `dispatch_preflight`, `writer_lease` to their run/provider/confinement owners if useful. |
| [PR #1332](https://github.com/mblauberg/provenant/pull/1332), head `f3fbb052cb56ebf3309ae0cfcc672990acd93c2a` | Typed store error/contention handling, transaction tests, `MessageStore`/`TaskStore` seams, injected clock/IDs and lease fencing | Store chair owns adoption. Strip `legacy.rs`, JS coercion/UTF-16/Number emulation and differential/Node harness. Workflow must not use its SQLite store as a receipt database. |
| [PR #1334](https://github.com/mblauberg/provenant/pull/1334), head `c02afccadea65d79474d8b9718dd0bdee57655a5` | Typed retained run facts, identity boundaries, bounded graph analysis and execution facade lessons | Runtime chair owns adoption. Remove Python owner subprocesses, ordered-dictionary/repr emulation, duplicated route defaults/alias maps and oracle helpers. Expose one verified attempt fact to workflow. |

## Rust-native verification

| Layer | Native cases and tools |
|---|---|
| Review/receipt | Duplicate actors, author/decision-maker exclusion, tier/lens/family fallback, unknown/ChairAttested/observed provenance, terminal adversarial counting, stale subject and required user approval. Minimal receipt bounds/version/revision. |
| CLI/hook | Envelope/status/1 MiB bound and 0/1/2/3/124/130 snapshots with injected IDs/clock and isolated roots; partial/input-required data, malformed input and side-effect-free help. SessionStart key/save template for present, absent, stale and malformed sessions; native advisory exits. No MCP fixtures. |
| Concurrency | Two fixture processes use handshakes to save the same revision: one winner, one conflict; independent delivery/session locks. No sleep-based assertions. |
| Faults | `cfg(test)` filesystem/clock fault traits inject read/write/rename/fsync/lock failure, ENOSPC and clock regression. Kill a fixture at publication boundaries; reread old/new complete bytes and distinguish failed durability from rollback. No `fail` crate. |
| Custody/subject | Symlink swap, traversal/non-regular files, SHA-1/SHA-256, changed document digest and wrong PR head. Git/process custody tests stay with supervisor/confinement; no workflow merge-tree validator. |
| Writing | Skill-owned rule data, Markdown/LaTeX exclusions, Unicode coordinates, overlaps, malformed/oversized patterns and legal blocking severity. |
| Product | Retired-name grep gate, new skill/hook callers, Python/Node absent from product PATH, clean-state install and rollback rehearsal on both Macs. CI owns full suites/stress/platform conformance. |

Use a test-only fixture binary for multi-process and kill-at-boundary tests.
Its integration-test launcher has a function-scoped
`#[allow(clippy::disallowed_methods)]` for `std::process::Command::new`, naming
ADR 0028 and fixture-only argv. Production launches remain in supervisor; any
signal/kill operation is supplied by supervisor test support. This is a scoped
allowance, not a new crate or a workspace-wide lint exemption.

Deliberate native mutants must fail: duplicate-review counting, treating
ChairAttested as cross-family, mismatched subject and omitted revision check.
Locally run only targeted tests and static checks. Exact-head CI gates any
future merge; this document grants no merge or cut-over authority.

## Deletion inventory and size budget

The inventory below distinguishes workflow-owned deletions from shared final
closure. A path is counted once. A mixed test is removed when its last caller
goes; until then delete only retired assertions and move remaining policy
coverage into the surviving Rust owner's tests. No test is reported as passing
because it was deleted. Counts are baseline physical lines, not estimates of
test coverage or future patch size.

### Workflow source

| File | Lines |
|---|---:|
| `skills/_shared/delivery_run_invariants.py` | 177 |
| `skills/_shared/portable_workflow.py` | 101 |
| `skills/_shared/review_ladder.py` | 117 |
| `skills/_shared/review_panel.py` | 277 |
| `skills/_shared/review_plan.py` | 421 |
| `skills/_shared/review_terminal.py` | 236 |
| `skills/_shared/run_gates.py` | 38 |
| `skills/autopilot/scripts/bootstrap-autopilot.py` | 516 |
| `skills/autopilot/scripts/validate_idle_pause.py` | 172 |
| `skills/deliver/scripts/authority_mapping.py` | 367 |
| `skills/deliver/scripts/delivery_policy_validation.py` | 437 |
| `skills/deliver/scripts/delivery_receipt.py` | 945 |
| `skills/deliver/scripts/delivery_receipt_commands.py` | 473 |
| `skills/deliver/scripts/delivery_receipt_process.py` | 123 |
| `skills/deliver/scripts/delivery_run_shape.py` | 189 |
| `skills/deliver/scripts/delivery_validation_artifacts.py` | 73 |
| `skills/deliver/scripts/delivery_validation_common.py` | 152 |
| `skills/deliver/scripts/delivery_validation_evidence.py` | 130 |
| `skills/deliver/scripts/delivery_validation_measures.py` | 220 |
| `skills/deliver/scripts/delivery_validation_reviews.py` | 146 |
| `skills/deliver/scripts/delivery_validation_security.py` | 207 |
| `skills/deliver/scripts/delivery_validation_shape.py` | 85 |
| `skills/deliver/scripts/reference_evaluation.py` | 419 |
| `skills/deliver/scripts/reference_runs.py` | 286 |
| `skills/deliver/scripts/select_security_evidence.py` | 77 |
| `skills/deliver/scripts/software_delivery_validation.py` | 350 |
| `skills/deliver/scripts/validate_delivery.py` | 206 |
| `skills/implement/scripts/bind_merged_delivery.py` | 292 |
| `skills/implement/scripts/checkpoint_run.py` | 145 |
| `skills/legal-writing/scripts/lint_legal_style.py` | 382 |
| `skills/natural-writing/scripts/check_academic_style.py` | 54 |
| `skills/natural-writing/scripts/style_lint.py` | 363 |
| `skills/orchestrate/scripts/check_herdr_cli.py` | 178 |
| `skills/session/scripts/cleanup_run_artifacts.py` | 202 |
| `skills/session/scripts/compact_reinject.py` | 169 |
| `skills/session/scripts/context_audit.py` | 305 |
| `skills/session/scripts/state_check.py` | 282 |
| `skills/setup-repo/templates/claude-tracker-hook.py` | 68 |
| `skills/tracker/scripts/steward.py` | 156 |
| `workflows/codebase-polish.js` | 568 |
| `workflows/cross-verify.js` | 455 |
| `workflows/implement-run.js` | 885 |
| `skills/setup-repo/templates/claude-tracker-settings.json` | 15 |
| `skills/deliver/templates/RUN.template.json` | 112 |
| `config/delivery-profiles.json` | 109 |
| `schemas/delivery-profiles.schema.json` | 174 |
| `evals/delivery-profile-scenarios.yaml` | 306 |
| `skills/deliver/portable-workflow.v1.json` | 1 |
| `skills/grill-me/portable-workflow.v1.json` | 1 |
| `skills/implement/portable-workflow.v1.json` | 1 |
| `skills/orchestrate/portable-workflow.v1.json` | 1 |
| `skills/release/portable-workflow.v1.json` | 1 |
| `skills/retrospect/portable-workflow.v1.json` | 1 |
| `skills/scope/portable-workflow.v1.json` | 1 |
| `skills/session/portable-workflow.v1.json` | 1 |
| `skills/work-map/portable-workflow.v1.json` | 1 |

### Workflow tests and interpreter adapters

| File | Lines |
|---|---:|
| `skills/implement/tests/test_checkpoint_run.py` | 164 |
| `tests/test_autopilot_bootstrap.py` | 226 |
| `tests/test_autopilot_pause.py` | 203 |
| `tests/test_context_audit.py` | 272 |
| `tests/test_context_resume.py` | 199 |
| `tests/test_delivery_contract.py` | 1,731 |
| `tests/test_delivery_receipt_producer.py` | 1,291 |
| `tests/test_delivery_validator_structure.py` | 32 |
| `tests/test_herdr_cli_contract.py` | 237 |
| `tests/test_herdr_orchestrate_contract.py` | 45 |
| `tests/test_implement_run_workflow.py` | 232 |
| `tests/test_legal_writing_portability.py` | 67 |
| `tests/test_review_ladder.py` | 133 |
| `tests/test_review_panel.py` | 221 |
| `tests/test_review_terminal.py` | 278 |
| `tests/test_run_artifact_cleanup.py` | 199 |
| `tests/test_run_invariants_characterisation.py` | 347 |
| `tests/test_security_evidence_selection.py` | 28 |
| `tests/test_session_state_check.py` | 509 |
| `tests/test_setup_repo_contract.py` | 68 |
| `tests/test_software_delivery_continuity.py` | 508 |
| `tests/test_specialist_cleanup.py` | 52 |
| `tests/test_tracker_placement_cases.py` | 31 |
| `tests/test_tracker_steward.py` | 118 |
| `tools/front-door/crates/workflow/tests/generate_policy_fixtures.py` | 61 |
| `tools/front-door/crates/workflow/tests/oracle.py` | 218 |

### UI runtimes, wrappers, tests and assets

| File | Lines |
|---|---:|
| `LICENSES/modern-screenshot-MIT.txt` | 9 |
| `runtime/ui-evidence/README.md` | 25 |
| `runtime/ui-evidence/build-browser-detector.mjs` | 160 |
| `runtime/ui-evidence/build-browser-detector.test.mjs` | 64 |
| `runtime/ui-evidence/detect.mjs` | 23 |
| `runtime/ui-evidence/detector/browser/injected/index.mjs` | 1,688 |
| `runtime/ui-evidence/detector/cli/main.mjs` | 347 |
| `runtime/ui-evidence/detector/detect-antipatterns-browser.js` | 4,035 |
| `runtime/ui-evidence/detector/detect-antipatterns.mjs` | 44 |
| `runtime/ui-evidence/detector/engines/browser/detect-url.mjs` | 296 |
| `runtime/ui-evidence/detector/engines/regex/detect-text.mjs` | 420 |
| `runtime/ui-evidence/detector/engines/static-html/css-cascade.mjs` | 954 |
| `runtime/ui-evidence/detector/engines/static-html/detect-html.mjs` | 179 |
| `runtime/ui-evidence/detector/engines/visual/screenshot-contrast.mjs` | 189 |
| `runtime/ui-evidence/detector/findings.mjs` | 12 |
| `runtime/ui-evidence/detector/node/file-system.mjs` | 229 |
| `runtime/ui-evidence/detector/profile/profiler.mjs` | 166 |
| `runtime/ui-evidence/detector/registry/antipatterns.mjs` | 278 |
| `runtime/ui-evidence/detector/rules/checks.mjs` | 1,948 |
| `runtime/ui-evidence/detector/shared/color.mjs` | 124 |
| `runtime/ui-evidence/detector/shared/constants.mjs` | 101 |
| `runtime/ui-evidence/detector/shared/page.mjs` | 7 |
| `runtime/ui-evidence/provenance_components.yaml` | 24 |
| `runtime/ui-live/README.md` | 11 |
| `runtime/ui-live/contained-source.mjs` | 397 |
| `runtime/ui-live/design-parser.mjs` | 820 |
| `runtime/ui-live/detect-csp.mjs` | 198 |
| `runtime/ui-live/impeccable-paths.mjs` | 214 |
| `runtime/ui-live/is-generated.mjs` | 69 |
| `runtime/ui-live/jsx-tag-scanner.mjs` | 583 |
| `runtime/ui-live/live-accept.mjs` | 706 |
| `runtime/ui-live/live-browser-session.js` | 199 |
| `runtime/ui-live/live-browser.js` | 5,241 |
| `runtime/ui-live/live-complete.mjs` | 119 |
| `runtime/ui-live/live-completion.mjs` | 18 |
| `runtime/ui-live/live-inject.mjs` | 732 |
| `runtime/ui-live/live-poll.mjs` | 250 |
| `runtime/ui-live/live-process.mjs` | 23 |
| `runtime/ui-live/live-resume.mjs` | 50 |
| `runtime/ui-live/live-server-startup.mjs` | 29 |
| `runtime/ui-live/live-server.mjs` | 1,824 |
| `runtime/ui-live/live-session-store.mjs` | 459 |
| `runtime/ui-live/live-status.mjs` | 86 |
| `runtime/ui-live/live-wrap.mjs` | 900 |
| `runtime/ui-live/live.mjs` | 403 |
| `runtime/ui-live/load-context.mjs` | 279 |
| `runtime/ui-live/modern-screenshot.umd.js` | 14 |
| `runtime/ui-live/provenance_components.yaml` | 68 |
| `runtime/ui-live/tests/live-server-startup.test.mjs` | 513 |
| `skills/ui-ux-design/scripts/detect-csp.mjs` | 5 |
| `skills/ui-ux-design/scripts/detect.mjs` | 16 |
| `skills/ui-ux-design/scripts/live-accept.mjs` | 5 |
| `skills/ui-ux-design/scripts/live-complete.mjs` | 5 |
| `skills/ui-ux-design/scripts/live-inject.mjs` | 5 |
| `skills/ui-ux-design/scripts/live-poll.mjs` | 5 |
| `skills/ui-ux-design/scripts/live-resume.mjs` | 5 |
| `skills/ui-ux-design/scripts/live-server.mjs` | 5 |
| `skills/ui-ux-design/scripts/live-status.mjs` | 5 |
| `skills/ui-ux-design/scripts/live-wrap.mjs` | 5 |
| `skills/ui-ux-design/scripts/live.mjs` | 7 |
| `skills/ui-ux-design/scripts/ui-evidence-paths.mjs` | 8 |
| `skills/ui-ux-design/scripts/ui-live-paths.mjs` | 8 |
| `skills/ui-ux-design/scripts/ui-runtime-paths.mjs` | 38 |
| `tests/test_frontend_context_loader.py` | 109 |
| `tests/test_frontend_live_process_security.py` | 160 |
| `tests/test_frontend_live_server_security.py` | 1,658 |
| `tests/test_frontend_source_security.py` | 2,228 |
| `tests/test_ui_ux_detector_cli.py` | 644 |
| `tests/ui_ux_live_test_support.py` | 159 |

### Other chair's deletions

These rows are excluded from workflow's deletion/net budget. Run owns the
entire run-directory contract, with a **Cut** recommendation: its files under
`runs/<RunId>/` plus `provenant clean` replace scaffolding, manifests and finalisation. Lane C
may implement against fakes but must not delete `review_plan.py` until run has
removed `run_dir_finalize.py` and rewritten its callers.

| Owner | File or contract | Baseline lines | Replacement/ordering |
|---|---|---:|---|
| run | `skills/orchestrate/scripts/git_evidence_packet.py` | 88 | Rewrite old dispatch/control callers to top-level `dispatch`, `lanes`, `output`, `cancel`, `resume` and `reconcile`; workflow owns neither. |
| run | `skills/orchestrate/scripts/run_dir_init.sh` | 138 | Cut the `.agent-run` scaffolder; use run-owned files. |
| run | `skills/orchestrate/scripts/run_dir_finalize.py` | 654 | Cut finaliser before lane C deletes its review-plan import. |
| run | `schemas/run-receipt.schema.json` | 751 | Cut old run receipt schema with finaliser/scaffolder. |
| run | `schemas/manifest-row.schema.json` | 55 | Cut manifest-row contract with its last run caller. |
| run | Generated `RUN_RECEIPT.json` and `MANIFEST.md` contract | Not tracked | Cut new writes; retain existing run files untouched. Rewrite implement run-contract and orchestrate memory-scratchpad instructions. |
| telemetry | `scripts/telemetry_changes.py` | 406 | Retire legacy change markers; `telemetry stats` reads terminal run records with prices. |
| telemetry | `scripts/telemetry_delivery.py` | 276 | Retire receipt ingestion; the receipt plus run records are the record. Excluded from workflow's budget. |

### Shared helpers and installer

These files disappear under their shared owners, outside workflow's budget.
Their callers move within those owners, without adapters.

| File | Lines | Owner |
|---|---:|---|
| `scripts/install-workflows` | 315 | config/install |
| `skills/_shared/bounded_process.py` | 230 | supervisor |
| `skills/_shared/custody.py` | 327 | confinement |
| `skills/_shared/excludes.py` | 24 | core |
| `skills/_shared/harness_profile.py` | 403 | config |
| `skills/_shared/roots.py` | 45 | config |
| `skills/_shared/workspace_paths.py` | 78 | confinement |

The final Node/runtime test closure below is coordinated with Fabric, config,
core, quota, telemetry and run. Root package files stay until the last Node
consumer is removed; the UI retirement alone does not own them. Tests of
surviving skill policy become native/static repository checks rather than being
discarded as private runtime tests. Dedicated UI tests above are not repeated.

### Card 49 and final package closure

| File | Lines |
|---|---:|
| `package-lock.json` | 3,287 |
| `package.json` | 26 |
| `runtime/fabric/live-sandbox-smoke.py` | 215 |
| `runtime/fabric/package.json` | 31 |
| `runtime/fabric/tests/atomic-write.d.mts` | 1 |
| `runtime/fabric/tests/atomic-write.mjs` | 10 |
| `runtime/fabric/tests/fixture-processes.test.ts` | 74 |
| `runtime/fabric/tests/helpers/fixture-processes.ts` | 68 |
| `runtime/fabric/tests/hermetic-tmpdir.ts` | 30 |
| `runtime/fabric/tests/spawn-fault-preload.mjs` | 24 |
| `runtime/fabric/tsconfig.json` | 14 |
| `scripts/check-skill-javascript` | 22 |
| `scripts/node-workspace-preflight.mjs` | 119 |
| `scripts/node-workspace-provision.py` | 130 |
| `tests/test_fabric_skill_params.py` | 78 |
| `tests/test_fabric_v1_contract.py` | 123 |
| `tests/test_harness_contract.py` | 484 |
| `tests/test_live_sandbox_smoke.py` | 124 |
| `tests/test_skill_audit_contract.py` | 48 |
| `tests/test_skill_reuse_rules.py` | 53 |
| `tests/test_static_skill_corrections.py` | 14 |
| `tests/test_support_helpers.py` | 98 |
| `tests/test_ui_ux_design_boundaries.py` | 42 |
| `tests/test_ui_ux_provenance_contract.py` | 213 |
| `tests/test_ui_ux_reference_contract.py` | 19 |
| `tools/front-door/crates/cli/src/pending/generate_unicode.py` | 24 |
| `tools/front-door/crates/core/scripts/generate_unicode.py` | 29 |
| `tools/front-door/crates/fabric-store/tests/oracle.mjs` | 162 |
| `tools/front-door/crates/quota/src/tests/oracle.py` | 84 |
| `tools/front-door/crates/telemetry/src/pending/python/generate_unicode.py` | 29 |

### Retained mixed callers and net lines

Edit these instead of deleting their surviving guarantees: the UI skill and
review/visual/live references; `skills/ui-ux-design/evals/provenance_components.yaml`;
`THIRD_PARTY_NOTICES.md`; architecture's UI paragraphs; installer/check-harness
registrations; the 10-line generated-file probe in `tests/test_pytest_sharding.py`;
and workflow-specific cases in install, command-doc, clean and telemetry tests.
Card 49's skill/provenance/reference tests move their retained SKILL.md policy
checks into native repository assurance. The Impeccable licence, NOTICE,
acknowledgements and skill prose remain; only the screenshot vendor notice goes.
Root package manifests are shared final deletions, not UI-owned packages.

| Baseline or target | Physical lines | Accounting |
|---|---:|---|
| Workflow legacy source | 12,169 | 56 files; excludes run's 88 and telemetry's 682 lines, includes omitted templates/config/schemas below. |
| Workflow tests and Python adapters | 7,470 | 26 files, including 279 interpreter-support lines. |
| UI runtimes, wrappers, tests and assets | 30,607 | 69 files, including generated/vendor code and licences. |
| **Legacy deletion subtotal** | 50,246 | Workflow-owned only; shared closures and edits to retained files excluded. |
| Generated or vendored code subset | 23,598 | Provenance ledger membership, detailed below; counted once even if generated and third-party. |
| Existing inert Rust/neutral fixtures | 8,863 | 8,771 source plus 92 neutral fixture lines, excluding the 279 above. No deletion credit where code may move owners. |
| New implementation, rule data and tests | about 3,250 | 1,450 production/adapters, 300 skill-rule JSON lines and 1,500 native test/fixture lines, including reused code. |
| **Net reduction including generated/vendor code** | **46,996** | 50,246 - 3,250; a design budget, not a measured implementation diff. |
| **Net reduction excluding generated/vendor code** | **23,398** | 50,246 - 23,598 - 3,250; new code is assumed hand-written. |

The full exclusion is 23,598 lines of generated or vendored/third-party-derived
source: 11,017 under the detector prefix and 12,581 in the ui-live third-party
components named by their `provenance_components.yaml` ledgers. The generated
detector bundle (4,035), modified third-party `live-browser.js` (5,241) and
vendored screenshot bundle (14) are a 9,290-line subset, not the full vendor
exclusion. Count the union once; licences and original harness wrappers/tests
remain in the hand-written total. This broader provenance-based subtraction
is why the second net figure differs from subtracting only the three bundles.

Count again after integration. Shared closures, existing Rust simplification,
this design document and replacement skill prose do not inflate the estimate.

## Caller rewrite map

Locations below are the pre-rewrite source lines, relative to `skills/` unless
prefixed `docs/`, `runtime/`, `schemas/` or `tests/`. Each row gives replacement text/verb
or deletion and its lane. All `*` verdicts above are covered here; deleting a
script also deletes its internal calls and dedicated tests in the inventory.
Verbs remain subject to the shared freeze. Rewrite surviving instructions in
the same lane that retires their target, without compatibility wrappers.

| Caller file:line | New text/verb or deletion | Owner |
|---|---|---|
| `deliver/SKILL.md:16-20`; `docs/specs/harness/profiles.md:41,49` | Delete five-profile/overlay/minimum-repetitions registry claims. State scoped criteria in the PR or skill document; evaluate owns repetition protocols. Keep repair ceilings as guidance. | B |
| `deliver/SKILL.md:25-43,63-65` | Replace RUN.json setup and validation with minimal `workflow receipt save` and `workflow review`; record subject/authors/risk/review attempt refs/decision. For PR work cite the PR and exact-head CI. | G |
| `deliver/SKILL.md:21-31`; `runtime/fabric/src/execution-input.ts:81,150,302-304,415` | Replace old path/`delivery_run_id` hand-off with typed `DeliveryId` carried in dispatch. No second run-directory receipt. | G prose; other chair run for dispatch |
| `deliver/references/contract.md:9-22,35-41,53-59` | Replace producer/accept/repair commands with minimal receipt save/review. Put findings and repair reasons in PR/document; retain 2/4/5/5 repair ceilings and return-to-scope rule as prose. | B profiles; G receipt/repair |
| `deliver/references/contract.md:124-175,179-197`; `deliver/SKILL.md:49-53,70-72` | Delete universal outcome/evaluation/observation rules and merge binder. “Evaluate and release own their evidence; the PR records exact-head CI and repository merge.” Preserve domain security/user approval duties. | G |
| `code-review/references/finding-contract.md:41` | Delete `delivery_receipt.py finding`; record severity, file:line, impact and fix in the PR or review artefact. | G |
| `implement/SKILL.md:65,74-75` | Replace `repair --failing` and validator call with PR repair notes and `workflow review`; preserve bounded repair and required checks. | G |
| `implement/references/run-contract.md:9-31,47,67` | Delete software profile/RUN.json validator, acceptance and `bind_merged_delivery` commands. “The PR is the evidence: exact-head CI, required review, then repository merge.” Session Markdown records progress. | G |
| `implement/references/run-contract.md:55`; `orchestrate/references/memory-scratchpad.md:16-74` | Delete init/finalise, MANIFEST/RUN_RECEIPT and custom run-directory instructions; use top-level `dispatch`, `lanes`, `output`, `cancel`, `resume`, `reconcile` and `provenant clean`. | Other chair run; C deletion follows |
| `orchestrate/scripts/dispatch_run.py:58,495`; `orchestrate/scripts/run_controls.py:37,354,367` | Delete git-evidence-packet imports/use; replace script callers with top-level `dispatch`, `lanes`, `output`, `cancel`, `resume` and `reconcile` through run. | Other chair run |
| `orchestrate/SKILL.md:70`; `orchestrate/references/autonomous-implementation.md:13-21,33-38,47-67` | Remove per-issue RUN.template/RUN.json mandates and receipt-path reporting. Track issue/PR and exact-head CI; minimal receipt uses DeliveryId where needed, session Markdown resumes work. Preserve readiness, authority, bounded repair and merge gates. | G orchestration/delivery prose; E resume; other chair run admission |
| `orchestrate/references/direct-cli-fallback.md:8` | Remove `run_dir_init.sh` and primary-checkout scratch writes; use top-level `dispatch` then `lanes`/`output`, with `cancel`, `resume` and `reconcile` for control/recovery. Run owns file custody. Preserve degraded-route declaration and authority checks. | Other chair run |
| `evaluate/references/receipt.md:3-7,20-22` | Replace enclosing delivery-run schema/path anchors with subject plus optional DeliveryId/PR reference; evaluate retains protocol, evidence and acceptance of its own evaluation result. No workflow Evaluation rule. | Evaluate skill owner, coordinated by G |
| `docs/runbooks/telemetry.md:175-195`; `tests/test_telemetry.py:320` | Replace RUN.json/delivery_acceptance/merge_commit joins, `--delivery-run-id` and RUN.template fixture with terminal run records carrying `DeliveryId` and minimal test constructors. `telemetry stats` applies prices; preserve missingness and receipt decision references without ledger ingestion. | Other chair telemetry; I integration |
| `docs/specs/harness/lifecycle.md:98,168-178,434` | Replace canonical delivery-run v1/RUN.json and all-gates kernel claims with minimal receipt and PR evidence; evaluate/release own domain evidence. Retain explicit user gates and frozen-install rule. | G lifecycle text; other chair run for retired run-directory contract |
| `tests/test_record_schemas.py:335-339,372` | Remove delivery-profiles registry/schema cases while preserving unrelated config/schema coverage; native minimal-record tests replace only retired workflow assertions. | B |
| `orchestrate/references/verification.md:134-158` | Delete panel preset/axis/membership/reducer registry instructions; request named independent lenses and synthesise their findings directly. | C |
| `orchestrate/references/orchestration-contract.md:7-22,29,75` | Remove accelerator bindings and run-directory ledger claim; native skill orchestration uses run-owned files and the session checkpoint. | H accelerator text; other chair run files |
| `orchestrate/references/dynamic-workflows.md:1` | Retire our three-accelerator adapter instructions; preserve host-independent review/authority guidance in orchestration-contract. No claim that Provenant owns Claude's Workflow host. | H |
| `grill-me/SKILL.md:37`; `orchestrate/SKILL.md:76`; `retrospect/SKILL.md:78`; `release/SKILL.md:84`; `scope/SKILL.md:88`; `work-map/SKILL.md:73` | Delete portable-wrapper schema links; point to the skill's existing project document or PR. No replacement context-stamping command. | B |
| `deliver/SKILL.md:91`; `implement/SKILL.md:101`; `session/SKILL.md:83` | Delete wrapper schema links; use minimal receipt for delivery/implementation and Markdown checkpoint for session. | B deletes wrappers; G/E rewrite text |
| `deliver/scripts/delivery_receipt.py:653-690`; `deliver/scripts/delivery_receipt_commands.py:324-333` | Delete evidence subprocess runner and its internal calls; retain results in the PR/document from run or an authorised command. | G |
| `tests/test_delivery_contract.py:1`; `tests/test_security_evidence_selection.py:1` | Delete synthetic builder/selector assumptions with their source; retain required security/evaluation obligations as domain policy tests. Reference builders have no replacement CLI. | B/G |
| `session/references/chair-state.md:64-110` | Replace state checker/install-hook/transcript instructions with `workflow session save <session-id> --file <checkpoint.md>` and SessionStart key injection. Managed hook changes occur at final install. | E |
| `session/references/context-hygiene.md:59,66` | Delete context audit/cleanup-script calls; save bounded checkpoint and refer retention to `provenant clean`, without old-root deletion authority. | E; J cleanup integration |
| `autopilot/SKILL.md:32`; `autopilot/references/state-contract.md:11-35,55-67` | Delete bootstrap/knob parser and Markdown queue/lease mechanics. Retain user mission/STOP constraints; use lanes/tasks for live work and session Markdown for resumption. | E |
| `autopilot/references/codex-operator.md:44,94`; `autopilot/references/recovery-and-cadence.md:17-18,137-143,163` | Delete idle validator and duplicate queue/reconcile/lease protocol; read top-level `lanes`, Fabric tasks and run admission; use `resume` and `reconcile` for continuation/recovery. Save blockers/resume trigger, never infer completion from idle. | E |
| `autopilot/templates/GOAL.template.md:9-15`; `autopilot/templates/STATE.template.md:19,57`; `autopilot/templates/README.template.md:26-35,66-69,106-108`; `autopilot/templates/HANDOFF.template.md:23-24`; `autopilot/templates/QUEUE.template.md:1-24` | Remove bootstrap, fixed state shape and queue/lease requirements. Keep optional mission/authority/handoff prose; remove live queue/lease tracking from the templates. | E |
| `tracker/SKILL.md:61-64`; `tracker/references/steward.md:29` | Delete replay queue/outcome writer; reconcile current issues/tasks with `gh` reads before retrying an authorised write. | H |
| `setup-repo/SKILL.md:77-78` | Delete tracker denying-hook/settings-template instructions and both templates; retain repository tracker routing guidance. | H |
| `tests/test_herdr_cli_contract.py:13`; `tests/test_herdr_orchestrate_contract.py:1` | Delete retired Herdr CLI contract checker/tests; any retained external transport belongs to that integration owner. No workflow Herdr gate. | H |
| `legal-writing/SKILL.md:73`; `legal-writing/references/validation-checklists.md:206,214,218`; `legal-writing/references/forbidden-patterns.md:9-14`; `legal-writing/references/australian-english-house-style.md:53-54` | Use `provenant check writing <paths> --rules legal`, with quoted-example/exclusion rules in skill data. Preserve filing-blocker failure severity and warnings as review prompts. | D |
| `natural-writing/references/academic-prose.md:310`; `natural-writing/scripts/check_academic_style.py:18`; `legal-writing/scripts/lint_legal_style.py:24` | Use `provenant check writing <paths> --rules academic`; delete Python engine imports. General/academic/legal rules move to skill-owned JSON. | D |
| `ui-ux-design/SKILL.md:3,22-23`; `ui-ux-design/references/review.md:25`; `ui-ux-design/references/visual-qa.md:25`; `ui-ux-design/references/live.md:10-22,38` | Remove live-iteration/detector promises and every live command. “Read-only UI review by default; pair source changes with implement and use external browser evidence with app-owned preview.” | H |
| `ui-ux-design/evals/boundary_cases.yaml:95-103`; `ui-ux-design/evals/regression_cases.yaml:79-85`; `ui-ux-design/evals/reference_disposition.yaml:26`; `ui-ux-design/evals/provenance_components.yaml:24` | Replace live/detector expectations and runtime dispositions with external-browser review cases. Preserve read-only authority and retained skill provenance. | H |
| `docs/runbooks/github-workflow.md:202,309-321,349` | Replace receipt-directory/binder/validator requirements with PR evidence and exact-head CI. Replace primary-checkout sync at merge with the frozen-install rule until cut-over. | G; J final install wording |
| `docs/ARCHITECTURE.md:88,165-197,292,413`; `schemas/README.md:46` | Replace delivery kernel/profile projection with minimal receipt/review; remove retired profile-schema index row and detector command. Preserve review/user gates and skill provenance. | B index; G lifecycle; H UI |

Lane J owns a CI **retired-script-name grep gate**. Generate the literal name
list from the approved deletion inventory, including the other chair's run and
telemetry scripts after their owners land. `rg -n -F -f <retired-script-names>`
must find zero active callers in skills, library, surviving runtime/CLI source,
managed hook/install templates, active runbooks and harness specs. Include
retained tests and documentation projections. Inventories/history such as this
design need an explicit path-scoped allowlist; do not blanket-exclude skills.
Also match retired schema/config/template names and flags, including
`RUN.template.json`, `delivery-profiles`, `--delivery-run-id` and old canonical
`.agent-run` RUN.json path/schema claims; script names alone miss these callers.
The gate runs after all text owners integrate and before cut-over. It catches
unlisted residual callers as well as the rows above.

## Implementation lanes

Each lane includes targeted native tests and review/repair. All work uses
worktrees from `origin/main`; the primary checkout on each Mac remains frozen
under the agreed v7 rule. H is the first deletion lane after A verifies that
rule on both Macs. Library work in disjoint modules can proceed in parallel;
caller deletions integrate only in the dependency order below.

| Lane | Days | Depends on | Deliverable, deletion and text ownership |
|---|---:|---|---|
| A. Contract freeze and live-install check | 1 | Both chairs | Verify frozen primary checkouts, record their revisions and live link inventory; freeze shared types, root, fact/FS contracts and commands. No symlink/settings edits. |
| H. UI and accelerator retirement | 1 | A | First pure deletion: UI sources/bundles/tests, JS accelerators, tracker hook/settings companion, steward and Herdr checker. Own UI/setup/tracker text and eval rewrites; config removes install registrations at cut-over. |
| B. Minimal records | 1 | A, records/config | Receipt/risk/review records and constructors; delete reference builders, portable schemas/stamper, RUN template and profile registry/schema/scenarios. Own deliver profile/portable text. |
| C. Review | 1.5 | A; run-directory contract retired before deletion | Pure coverage validator and adversarial tests; delete ladder/plan/panel/terminal Python and route-map copies. Own panel/review caller text. |
| D. Writing | 1 | A, lint command freeze | Small matcher and skill-owned JSON rules; delete three Python linters and retired wrapper tests. Own legal/natural writing instructions. |
| E. Session/hooks | 1 | A; safe-FS/host-session interfaces | Markdown checkpoint and key/save-command injection, built against fake handles/host events first. Delete session/autopilot helpers and idle validator. Own session/autopilot text; no run/runtime frontier dependency. |
| F. Store | 1 | B, confinement/supervisor | Lock/revision/atomic replacement and fault tests; delete producer write/process modules. |
| G. Receipt/review binding | 1 | B/C/F; run fact interface | Minimal decision and review-attempt binding against fake run facts first. Delete validator cone and old `skills/implement` merge-binder/checkpoint scripts; E owns replacement checkpoints. Own deliver/implement evidence/repair/merge text. |
| I. CLI/telemetry integration | 1.5 | C/D/E/G, real run/FS/host APIs, command freeze | Replace fakes with real owner reads; CLI/hook snapshots, run `DeliveryId` links and terminal-record stats with prices. No telemetry ledger or summary ingestion. No MCP adapter. |
| J. Closure and final install | 1.5 | H/I, all product owners | Own final retired-name CI grep gate, mixed docs/tests and package closure; clean-state native gates and final link rewrite/install on both Macs. |
| **Total** | **11.5** | Shared owner effort excluded | Integration waits never justify compatibility layers or pulling the frozen primary. |

At J, use the table's `install [--dry-run]`, `check install` and `uninstall` commands,
with `--legacy-checkout ABS`. Re-applying `install` is recovery; rollback
reinstalls `pre-rust-rewrite`. The compiled cut-over link/command inventory
belongs in [state.md's installer section](https://github.com/mblauberg/provenant/blob/docs/rewrite-state/docs/specs/rust-port/design/state.md#proposed-commands-and-installer-behaviour),
whose older command proposal must conform to the table. State matches exact
commands/link targets beneath the supplied checkout, reads links with
`readlink` only and prints every harness-config change before applying it.
Keep only workflow's entries here:

| Workflow surface | Final install action |
|---|---|
| `~/.claude/workflows/codebase-polish.js`, `cross-verify.js`, `implement-run.js` | Remove the three managed checkout-targeting links; no replacement workflow host. |
| `~/.claude/settings.json`, SessionStart command | Replace `python3 "<primary>/skills/session/scripts/compact_reinject.py"` with `provenant hook claude compact`. |
| Same file, PreCompact command | Replace `python3 "<primary>/skills/session/scripts/state_check.py" --hook` with `provenant hook claude pre-compact`. |
| Tracker denying-hook/settings companion | Remove its exact managed registration; preserve unrelated/user-owned settings. |

State owns the complete skill/MCP/binary/instruction-link inventory on each
Mac. At cut-over it may automatically update `~/.agents/AGENTS.md` and files
linked to it when switching to `PROVENANT_AGENT_LABEL`, as authorised in the
table on 24 October 2026. Until then the primary install stays frozen. Rehearse
install/rollback with isolated roots, then require exact-head CI and final
cut-over authority; this conform pass does not perform a live install.

## Open decisions

| Binding topic | Proposed table/owner change | Current design constraint |
|---|---|---|
| Layout | Add workflow's subtree: `workflow/projects/<ProjectId>/deliveries/<DeliveryId>/receipt.{json,lock}` and `sessions/<SessionId>/checkpoint.{md,lock}`. | A subtree of the single host-global root, not a per-project root; no alternate root pending acceptance. |

## Cross-crate asks for the other chair

Proposed interfaces below need owner acceptance; they are not claims about
currently exposed APIs. Composition-edge code gathers facts before invoking
workflow. No cross-crate private-constructor or reverse-dependency guarantee.

| Owner crate/host | Ask and boundary |
|---|---|
| core | `ProjectId`, `DeliveryId`, `SessionId`, `AttemptId`, `ActorId`, `RepoId`, `GitOid` (SHA-1/SHA-256), `Digest`, `UtcMicros`, `Deadline`, `CancelToken`, `RelPath::parse`; production `CommitClock` via `jiff`. No `WorkScope` ask after cutting idle validation. |
| records | Bounded decode, canonical encode (sorted keys, no final LF; disk files may append LF), digest; minimal receipt, subject, risk/review policy and request records; `RouteProvenance`/`ReviewEligibility` with Observed/ChairAttested/Unknown. Facts serialise but do not deserialise. No checkpoint or writing-rule schema. |
| config/state | Typed risk/review settings and bounds from one root-owned `config.toml` plus compiled defaults, with optional `profile =`; no layers/project overlays/per-knob env or mutation verbs. Root `~/.local/share/provenant/v1`, `--state-dir` over `PROVENANT_STATE_DIR`; accept workflow subtree row. Own `install [--dry-run]`, `check install`, `uninstall`, `--legacy-checkout` inventory and authorised instruction updates; preserve unrelated/user-owned entries. Writing data stays with skills. |
| supervisor | Safe FS: stable lock inode, no-follow open, atomic replace/no-replace, file/directory sync; expose bounded `FileHandle::lock(LockOptions)` polling non-blocking `flock` through confinement, with cancellation. `run_utility` for fixed git/gh reads. Test-only fixture launch/kill support. |
| confinement | Own `Grant`, `Workspace`, `RepoHandle`; `open_workflow_store`, `open_artifact`, `narrow`, safe `git_facts` with replacement objects off/alternates ignored. `StoreRoot::lock(..., Deadline, CancelToken)` returns a held record for bounded `read`/atomic `publish`; ordinary path/phase/cause I/O diagnostics, no workflow uncertain-commit protocol. |
| run | `AttemptReader::attempt(AttemptId) -> AttemptFact` (fact type in `records`, `Serialize` only; see execution-run.md): terminal state/exit, subject, output ref/digest, route and reviewer. Terminal records carry `DeliveryId` for stats. Own top-level `dispatch`, `lanes`, `output`, `cancel`, `resume`, `reconcile`, git-evidence helper and entire run-directory deletion contract; remove finaliser before C deletes its import. No work-snapshot/frontier ask. |
| providers | `validate_provenance(&RouteProvenance) -> VerifiedLineage`; keep adapter, endpoint and model family separate. ChairAttested never fills cross-family coverage. Workflow consumes edge-built facts, without a providers dependency. |
| routing | `review_availability(&RoutingSnapshot, &ReviewDemand) -> ReviewAvailability`: tiers, all author families, expiry and quota/config/capability/family-slot blockers. No probes under the receipt lock. Quota feeds routing, with no workflow call. |
| telemetry | No workflow write/report API or ledger. `telemetry stats` reads terminal run records with prices; receipt plus run records remain the record. Keep old ledger on disk without import/deletion; retire OTel at cut-over. |
| cli/binary | Parse proposed workflow leaves and `hook claude pre-compact\|compact`; route `check writing` to workflow's matcher; register workflow's pre-compact and session-start handlers in cli's `HookSpec` table, which the installer also reads. Binary composes typed owner requests and gathers subject/attempt/actor/routing facts; no MergeFact or library-to-cli edge. Render CLI envelope, 1 MiB cap and exits 0/1/2/3/124/130; partial/input-required in data. Run verbs stay top level under run. |
| fabric-store / fabric-runtime | Own the only read-only one-shot cut-over import: open/blocked tasks plus unacknowledged requests from the last 14 days. Map project/identity without importing authority; workflow points agents to it. Product identity env is `PROVENANT_AGENT_LABEL`, replacing `AGENT_FABRIC_LABEL`. |
| fabric-mcp | No workflow asks, tools or dependency edge. CLI JSON, top-level run `lanes` and Fabric tasks suffice. |
| Native host | Supply session ID in hook events; SessionStart injects actual key/save command. Retain observed launch identity when available, otherwise explicit ChairAttested native provenance under the limits above. |
