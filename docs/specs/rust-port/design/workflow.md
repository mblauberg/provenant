# Workflow Rust design

## Decision and boundary

Replace the workflow machinery with one small synchronous policy library, a
revisioned delivery receipt and a session checkpoint. Keep checks that bind
authority, evidence, review and acceptance to an exact outcome. Cut workflow
engines, generated reference runs, duplicated queues and the entire bespoke UI
runtime. Skills conduct the work using `git`, `gh`, Fabric and browser tools.

This is a design for both implementation chairs, not an implementation or
cut-over approval. The 2026-10-23 v7 direction supersedes the transition,
compatibility and parity sections of [ADR 0028](../../../adr/0028-rust-port-dependencies.md),
[PORTING v5](../PORTING.md) and the [port plan](../README.md).
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
| MCP clients | Obtain structured results without scraping terminal prose | Fabric currently exposes dispatch and retained run reads, not delivery/writing tools (`runtime/fabric/src/server.ts`). Add two small adapters to the same library entry points. |
| Harness hooks | Restore the correct session after compaction | `session/scripts/state_check.py:84` emits PreCompact and SessionStart hook configuration. Keep the host's JSON stdin/stdout contract; replace transcript searching with an explicit checkpoint key. |
| Long-running skill | Resume a goal and distinguish idle waiting from completion | `autopilot/SKILL.md:32`, templates/README:68 and `validate_idle_pause.py` use a second Markdown queue. Preserve pause checks; use Fabric/run facts instead of another scheduler. |
| Writing skills | Locate mechanical prose defects, including legal/academic rules | `legal-writing/SKILL.md:73`, `natural-writing/references/academic-prose.md:310`. Retain one matcher with three rule sets. |
| Repository setup and tracker | Follow the declared tracker and avoid duplicate issue work | `setup-repo/SKILL.md:76` offers the shell-detecting hook; `tracker/references/steward.md:29` offers replay bookkeeping. Replace with skill instructions and existing tasks plus `gh` reads. |
| Installed workflow host | Discover `meta` and execute `run` JavaScript exports | `scripts/install-workflows:46` globs sources; `scripts/install-harness:366,556` installs them. These are real surfaces even without literal imports. Retire the accelerators rather than build a Rust host. |
| Other crates and hosts | Link a delivery to attempts and report its outcome | `runtime/fabric/src/execution-input.ts` carries `delivery_run_id`; `scripts/telemetry_delivery.py:37` ingests receipts. Share typed identities and summaries, not executable workflow internals. |

Paths beginning with a skill name in this document are relative to `skills/`.
Evidence of a call in a skill is evidence of a supported path, not a measured
invocation count. State sampling is read-only, bounded and excludes prompt,
message and credential contents.

The default telemetry ledger at
`~/.local/state/agent-harness/fabric/telemetry.sqlite3` exists: about 565 MB,
566,185 usage rows, 566,877 usage-file rows, 111,191 observations, 46,923 events,
13 deliveries and zero changes at inspection. This proves substantial usage
history and a small populated delivery projection. It does not establish which
of the 13 deliveries were accepted or how often a particular helper ran.
No telemetry database was found under `~/.local/state/provenant`.

## Feature verdicts

`*` marks a user-visible removal or intentional change. Policy text and caller
instructions change with their owner; an obsolete script path is not retained
as a compatibility wrapper.

| Feature | Verdict | Reason and replacement |
|---|---|---|
| Risk derivation and explicit downgrade evidence | Keep | Derive the minimum from typed factors; lowering it still requires user authority. |
| Authority mapping and delegation containment | Merge | `confinement` owns grants; workflow binds an immutable grant reference and approval evidence. |
| Review ladder, plan, terminal and delivery review validators | Merge | One typed review model checks actual attempt, verdict, subject and independence once. |
| Panel presets, axis overrides, membership and reducer registry | Cut* | Skills can request lenses and synthesise findings; panel metadata adds no assurance beyond individual reviews. |
| Provider-family maps in workflow | Cut | Providers/routing own verified lineage; workflow must never guess a family from a name. |
| Portable-workflow context stamper and per-skill wrapper schemas | Cut* | A completed context stamp proves no outcome; attach an artefact reference to the receipt or use the skill's document. |
| Flat approval predicates and closed-run definitions | Merge | One state transition validator replaces string walks and the current release-dependent closure rule. |
| Five delivery profiles, overlays and policy callback dictionaries | Simplify* | Retain named requirement presets as data; one `Requirement` enum replaces nested executable registries. |
| Artefact custody, hashes, source scope and exact Git subject | Keep | These prevent stale or substituted evidence from certifying a different outcome. |
| Evaluation bindings and security evidence | Simplify | Bind typed evaluation/check receipts to declared requirements; keep source/authority/privacy requirements without a second evaluation engine. |
| Outcome and trajectory measure framework | Simplify* | Keep acceptance criteria with evidence references; cut universal trajectory metrics and repeated value/target/aggregation records. |
| Receipt mutation commands and repair/finding sidecars | Merge* | One CAS update accepts a typed operation batch; repairs and findings live in the same receipt. |
| Evidence subprocess runner in delivery | Cut* | Execute through `run` or an authorised ordinary command, then attach its typed result. No second custody owner. |
| Reference run/evaluation builders and security selector | Cut* | Rust test constructors cover the new design; synthetic passing records are not a product feature. |
| Merged delivery binder and implementation checkpoints | Merge* | Attach a verified merge fact and typed checkpoint through ordinary receipt operations. |
| Session Markdown shape checks, audit and transcript reinjection | Simplify* | Store a bounded typed checkpoint; restore only that host session, with explicit missing/stale status. |
| Scratch cleanup from session helper | Cut* | Retention belongs to the shared cleanup owner; no workflow deletion command or old-root traversal. |
| Mission scaffolder, knobs parser and Markdown item leases | Cut* | Skill templates remain optional documents; Fabric tasks and run admission own work and leases. |
| Idle-frontier pause validator | Simplify* | Validate a fresh scoped frontier snapshot; run owns any actual pause transition. |
| Tracker replay queue and outcome writer | Cut* | A file write cannot make a remote GitHub side effect exactly once; reconcile issues/tasks before retry. |
| Tracker Bash command parser and denying hook | Cut* | Shell recognition is incomplete; repository tracker instructions and command wrappers own routing. |
| Herdr CLI markdown/help contract checker | Cut* | Workflow no longer invokes Herdr; the external integration owner tests any retained transport itself. |
| JS implement-run, cross-verify and codebase-polish engines | Cut* | Existing skills plus Fabric already conduct these sequences; no replacement generic workflow interpreter. |
| Shared root, exclude, profile, bounded process and custody helpers | Merge | Core/config/confinement/supervisor own these services; no workflow copies. |
| Writing, academic and legal linters | Merge* | One scanner, common exclusions and declarative overlays; no Python regex object API. |
| Delivery telemetry, cost per accepted outcome and retrospective inputs | Simplify | Emit one summary per receipt revision; telemetry owns joins, deduplication and honest missingness. |
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
removes `/implement-run`, `/cross-verify` and `/codebase-polish`.

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
Allowed dependencies remain core, records, config and confinement. Its own
external crates are serde, serde_json, thiserror, regex and sha2; use the shared
timestamp type rather than add an independent clock. No SQLite in workflow.

| Module | Responsibility | Approximate production lines |
|---|---|---:|
| `review` | Independence, ladder coverage, route eligibility, terminal verdict and subject binding | 400 |
| `delivery` | Risk/requirements, acceptance, release and closure transitions | 700 |
| `evidence` | Artefact references, evaluation/check facts and merge continuity | 450 |
| `store` | Bounded read, revision check, publication and idempotency | 300 |
| `session` | Checkpoint validity, freshness, pause eligibility and hook context rendering | 250 |
| `writing` | Span exclusion, phrase/pattern matching and three rule sets | 650 |
| `error`, public API | Typed errors and transport-neutral request/report types | 250 |
| CLI and MCP adapters, outside the crate | Parse, gather owner facts, render and invoke | 500 |
| **Target** | Includes adapters, excludes shared owner implementations | **3,500** |

Records owns persisted/wire structs, `WorkflowPolicy`, `WritingRules`, request
types and schema versions; workflow owns their semantic checks. Config can
construct those records without importing workflow. Avoid a second set of
serialisable wrapper models.
All collections are bounded; use typed IDs and `BTreeMap` for keyed records.

| Type | Fields and invariant |
|---|---|
| `Subject` | `Files { artefacts: Vec<ArtifactRef> }` or `Git { repository: RepoId, commit: GitOid, tree: GitOid }`; evidence names this exact subject digest. |
| `ArtifactRef` | `id`, `location: Blob(Digest) | ProjectPath(RelPath) | GitObject(GitOid)`, media type, digest, byte length. Readable bytes must agree; a URI alone cannot pass a gate. |
| `DeliveryReceipt` | version, delivery/project IDs, revision, subject, author identities, immutable risk assessment, grant reference, requirements, evidence, reviews, findings, repairs, decisions, state, optional attempt links. |
| `Requirement` | Tagged `Check`, `Review`, `Evaluation`, `Approval` or `Observation`, each with ID, `stage: Accept | Release | Close`, subject and evidence constraints. Presets expand to these; project additions may strengthen them. |
| `Evidence` | Tagged `CheckResult`, `EvaluationRef`, `ApprovalRef`, `MergeFact` or `ObservationRef`; ID, subject, source digest, observed time. Each kind validates its own payload. |
| `Review` | ID, reviewer identity, actual route provenance, routing tier, named lens, subject, attempt/output references, verdict and findings. Unavailable legs carry a typed reason and referenced availability fact. |
| `Decision` | Tagged accept, release-recorded, close or abandon; actor, subject, timestamp, evidence references and acceptance basis where applicable. Recording release never performs it. |
| `SessionCheckpoint` | Version, project/session IDs, revision, goal and authority references, delivery IDs, summary, next actions, blocker references, resume triggers, observed frontier and update time. |
| `ValidationReport` | Subject/revision, sorted `Diagnostic { code, severity, field, evidence_id, message, remedy }`, status and missing requirements. Never turns a warning into implicit approval. |
| `Update` | Expected revision, operation ID and typed non-empty operation batch. Trusted actor context comes separately from the host. Accepted operations are all-or-nothing. |

Review pass requires a successful completed attempt, retained matching verdict
and output, verified route provenance, adequate tier, and the exact reviewed
subject. Distinct reviewer identities cannot include an author or decision
maker. Repeating a reviewer does not add coverage. Preserve the constitutional
routine/substantial/crucial/terminal ladder, distinct lenses and adversarial
terminal lens. A failed review is not an availability blocker. Required legs
remain required when a family is unavailable; a recorded same-family
replacement still needs independence and a different lens. Endpoint provider,
model family and review-eligible family remain separate typed fields.

`approve_with_nits` is passing only when every remaining finding is advisory;
an unresolved blocking finding prevents acceptance. Pool tiers come from the
attempt's resolved route, never a catalogue name or guessed model strength.
An unknown native route may contribute findings, but cannot fill a route-bound
review requirement. Native host adapters must supply the observed identity.

| Risk | Counted independent passing reviews | Required coverage |
|---|---:|---|
| Routine | 0 | Chair plus objective checks. |
| Substantial | 1 | One other-family reviewer if available, otherwise a recorded shortfall and one same-family reviewer through a distinct lens. |
| Crucial | 2 | Distinct lenses; two other families if available. Each missing other-family leg requires a recorded shortfall and independent same-family replacement. |
| Terminal | 2 | Crucial coverage plus an adversarial lens and explicit user acceptance. |

Every counted reviewer is at least the highest resolved author tier. A blocked
family is justified by a fresh routing availability fact, not an author-supplied
reason string. Missing-family slots may be recorded without inventing a family
identity when no such configured family exists. Failed work, missing verdicts
or unresolved findings cannot masquerade as unavailable providers.

`Author { actor: ActorId, kind: User | Agent, route: Option<RouteRef> }` binds
agent authors to retained verified attempt/host provenance. An unverified agent
author tier blocks substantial-or-higher acceptance; it is never guessed.
`VerifiedActorContext { actor, kind, host_session, provenance: ArtifactRef,
route: Option<VerifiedRouteRef>, grant: GrantRef }` comes from the host adapter.
The review policy input is `ReviewContext { authors: Vec<VerifiedAuthor>,
decision_makers: Vec<ActorId>, chair: VerifiedActorContext,
availability: ReviewAvailability }`. Routing's availability function therefore
takes `(&RoutingSnapshot, &ReviewDemand)` where demand includes highest agent
author tier, all author families, chair family and required other-family count.
For multiple author families, a reviewer must be independent of every author;
family separation is measured against all author families. Include the chair's
actor ID in decision makers and therefore exclude it from counted reviewers.
This follows HARNESS.md's author-relative ladder
and fixes the old helper's `chair_family` counting when chair and author differ.
Missing author provenance blocks coverage; no family is inferred. Add a native
regression with a chair family different from the author's.

| Transition | Required facts |
|---|---|
| Create to `Open` | Non-empty intent/criteria, bounded authority, derived risk and subject. |
| `Open` to `Accepted` | All Accept-stage requirements pass on current subject; explicit user acceptance for terminal/scoped work, otherwise checks/review or verified project landing. |
| `Accepted` to `Closed` | No promotion declared; completion records the accepted subject without demanding release approval. |
| `Accepted` to `Released` | Release-stage requirements, separate user release authority, declared observation plan and retained external result for that subject; workflow only records the result. |
| `Released` to `Closed` | Close-stage requirements and declared observation window settled by evidence. |
| Open/accepted to `Abandoned` | Explicit reason and actor; does not erase evidence or cancel processes. |

Changing a subject invalidates its acceptance and passing evidence for the new
subject. Preserve old decisions as history; explicitly reopen with new
requirements. Authority/risk cannot be rewritten by a checkpoint or review.
New risk factors can raise the minimum; lowering it binds explicit approval.
Checks cannot fabricate user consent: an `ApprovalRef` retains its originating
host/user evidence, action, scope, expiry and digest. Local records are an audit
of claims, not authentication of the person who typed a JSON field.

## Persistence, concurrency and errors

Default to a fresh root, proposed `${XDG_STATE_HOME:-~/.local/state}/provenant-rust`.
The config chair freezes its final spelling. Under it:

```text
workflow/projects/<project-id>/
  deliveries/<delivery-id>/receipt.json
  deliveries/<delivery-id>/receipt.lock
  sessions/<host-session-id>/checkpoint.json
  sessions/<host-session-id>/checkpoint.lock
  blobs/sha256/<digest>
```

Do not write `.agent-run`, `~/.agents` or old telemetry/transcript directories.
Project/session IDs are validated opaque components, never arbitrary paths.
Hold directory capabilities supplied by confinement. Project paths use UTF-8
relative components with no `..`; OS paths remain `Path`/`OsStr` at the edge.
An unrepresentable path returns a typed refusal rather than lossy hashing.
Git OIDs support both 40- and 64-digit forms with matching object format.

`receipt.json` is the sole committed state for that delivery, including its
decision history, findings and repair records. Do not add a parallel event
log, audit sidecar, accepted marker or index that must commit atomically with
it. No directory scan reconstructs current delivery state. Blob names bind
raw bytes; canonical record encoding sorts object keys with explicit version,
integer-only timestamps in UTC microseconds, UTF-8 and a final LF. Digests
cover precisely those bytes. Reject duplicate fields, unknown version/tags,
oversized input, non-finite numbers and out-of-range integers.

The new `schemas/workflow/` schemas describe closed version-1 records below,
with closed tagged unions for each nested type. They specify the new design;
they are not schemas copied from or compared with old RUN.json.

```rust
struct DeliveryReceiptV1 {
    version: u16, // exactly 1
    id: DeliveryId, project: ProjectId, revision: u64,
    intent: ArtifactRef, subject: Subject, authors: Vec<Author>,
    risk: RiskAssessment, authority: GrantRef, policy: Digest,
    requirements: BTreeMap<RequirementId, Requirement>,
    evidence: BTreeMap<EvidenceId, Evidence>,
    reviews: BTreeMap<ReviewId, Review>,
    findings: BTreeMap<FindingId, Finding>, repairs: Vec<Repair>,
    decisions: Vec<Decision>, state: DeliveryState,
    checkpoint: Option<DeliveryCheckpoint>, attempts: Vec<AttemptId>,
    operations: BTreeMap<OperationId, CommittedOperation>,
    created_at: UtcMicros, updated_at: UtcMicros,
}
// CommittedOperation = request Digest + committed revision; revision starts at 1.
// SessionCheckpointV1 uses the fields in the core-model table and the same
// revision/operation-id publication rules. Required fields have no defaults.
```

`RequirementV1 { id, stage, criterion: NonEmptyString, subject: Digest,
rule: RequirementRule, evidence: Vec<EvidenceId> }` is the complete requirement
shape. `RequirementRule` is a closed tagged union:

| Rule | Required payload and passing condition |
|---|---|
| `Check` | `check_id: CheckId`; a verified successful `CheckResult` with that ID and exact subject. The external check owns its method; criterion prose is never executed as a DSL. |
| `Review` | `lens: NonEmptyString`, `minimum: u8`, optional qualified-review artefact reference; independently verified passing reviews meet this criterion and the constitutional floor. |
| `Evaluation` | `protocol: ArtifactRef`, `minimum_runs: u16`, `minimum_sample: u32`; a passing typed evaluation receipt binds protocol/subject and satisfies both positive minima. |
| `Approval` | `action: ActionClass`, `scope: ScopeRef`; a verified originating user/host approval covers the action, subject, scope and decision time. |
| `Observation` | `plan: ArtifactRef`, `started_at: UtcMicros`, `minimum_duration_ms: u64`; a verified result covers the declared window and thresholds from that plan. |

Pin the expanded requirements and policy bytes as immutable blobs at creation.
Later configuration cannot silently weaken a receipt. A changed requirement
set needs explicit reopen/scope approval; the constitutional floor always
applies. `check` reports readiness separately for Accept, Release and Close.

Acquire the stable lock inode, read and check the expected revision, evaluate
the operation batch and bounds, sync referenced new blobs, then write/sync a
same-directory temporary receipt, replace and sync the directory. Locks are
held only for local reads/validation/publication, never provider or GitHub
waits. Prepare expensive owner facts beforehand, then recheck their subject
and freshness under the lock. Immutable blobs use no-replace publication;
an existing digest must match bytes. An interrupted blob write may leave an
unreferenced blob; no automatic deletion is needed for correctness.

Before acceptance, materialise every file-based canonical outcome and its
gating evidence into immutable blobs or verified Git objects. A mutable
`ProjectPath` is an input locator, not the accepted identity. Subsequent source
edits can make the working tree differ from the accepted subject; they cannot
change the bytes already certified. `VerifiedFact<T>` has private construction
through owner verification, never direct deserialisation from MCP arguments.

Each committed update stores its operation ID and request digest in bounded
receipt history. Repeating that pair returns the committed revision; reusing
an ID for different bytes refuses. When the history limit is reached, refuse
further mutation with an explicit capacity diagnostic rather than forget
idempotency keys. A new delivery can reference the closed predecessor.
After a failed post-replace directory sync, return `CommitUncertain` with
operation ID; reread/retry resolves it. Never report rollback or success
without evidence. Do not rely on `Drop` after abort/SIGKILL.

Library functions are synchronous. CLI performs one request; MCP owns bounded
transport workers and cancellation. Workflow starts no threads, daemon,
scheduler, HTTP listener or child process. A cancelled validation stops at
bounded read/hash checkpoints. Before publication it has no committed effect;
after publication begins it completes the local atomic operation and returns
its revision even if transport delivery is cancelled. Retry uses operation ID.
Cancellation never signals an attempt or releases another owner's lease.

Limits: receipt/request 1 MiB; checkpoint 32 KiB; hook output 8 KiB; at most
256 requirements, 256 reviews, 2,048 evidence items and 4,096 updates per
receipt. Hash artefacts as bounded streams, default aggregate budget 256 MiB
and explicit deadline; a larger authorised budget is supplied by the host.
Do not load arbitrary artefacts entirely into memory. Errors retain path and
OS cause but redact secret content. Library `WorkflowError` variants include
`Malformed`, `UnsupportedVersion`, `Limit`, `MissingEvidence`, `StaleSubject`,
`AuthorityDenied`, `Conflict`, `Contended`, `Cancelled`, `Deadline`, `Io` and
`CommitUncertain`. Policy failures are diagnostics, not panics.

Configuration is one immutable `WorkflowPolicy` snapshot from config: named
requirement presets, constitutional risk/review policy, writing rule sets,
limits and stale-checkpoint duration. No runtime registry mutation, per-helper
environment variables, global monkeypatching or dynamic regex code loading.
Use Rust regex syntax and literal phrase rules; compile overlays once with a
bounded pattern count/size. Report unsupported patterns as configuration
errors. Writing output uses one-based Unicode scalar line/column positions,
byte ranges for editors and deterministic rule IDs. Exclude code fences,
inline code and declared LaTeX command/math spans, with explicit profile tests.

## CLI, MCP and hook surface

Proposed subtree, **pending the shared command-tree freeze**:

```text
provenant workflow check <delivery-id|session-id> --kind delivery|session [--json]
provenant workflow receipt init --request <file|->
provenant workflow receipt show <id> [--json]
provenant workflow receipt update <id> --request <file|->
provenant workflow session save <id> --request <file|->
provenant workflow session show <id> [--json]
provenant workflow lint <paths...> --profile general|academic|legal [--json]
provenant workflow hook pre-compact|session-start
```

Updates use tagged operations `AttachEvidence`, `AddReview`, `AddFinding`,
`ResolveFinding`, `RecordRepair`, `SetCheckpoint`, `Accept`, `RecordRelease`,
`Close`, `Abandon` and `Reopen`. Immutable identity/risk/grant fields have no
generic JSON-patch escape hatch. `SetCheckpoint` records delivery progress;
session checkpoint updates use the separate session endpoint. `init` accepts
an expanded preset and approved scope, not flags for every nested field.
No `workflow run`, `panel`, `mission`, `tracker`, `ui` or reference-builder verb.

`check` verifies live artefact bytes and supplied typed owner facts by default;
offline incomplete evidence reports `unverified` and cannot enable `Accept`.
`show` does not claim current verification. Normal CLI prints a short state
and missing requirements; JSON returns `{ version, result, diagnostics }`.
Proposed exits: 0 valid/committed, 1 policy unsatisfied, 2 bad request/config,
3 conflict/contention, 4 I/O/uncertain commit, 130 cancelled. Errors go to stderr
in text mode; machine mode retains structured diagnostics with no prose on
stdout. Freeze these with CLI, rather than preserve accidental old exits.

Two MCP tools, also pending the shared freeze:

| Tool | Typed request | Result |
|---|---|---|
| `workflow_check` | Tagged `Delivery { id }`, `Session { id }`, `Writing { paths, profile }`, plus project context and bounded options | `ValidationReport`, receipt subject/revision or lint findings; no writes. |
| `workflow_update` | Tagged `CreateDelivery`, `UpdateDelivery` or `SaveSession`; update includes expected revision and operation ID; actor context comes from the host | Committed ID/revision/digest or typed conflict; no process execution. |

MCP schemas are closed tagged unions, bounded input and explicit versions.
Keep MCP protocol errors separate from `isError` tool failures and ordinary
unsatisfied-policy reports. Never place diagnostics on MCP stdout outside
JSON-RPC. Authorisation comes from host context and retained scope evidence,
not from a tool argument granting itself permission.

Propose one internal layering amendment: permit `fabric-mcp -> workflow` for
these two adapters. It removes a CLI subprocess or a duplicated runtime
facade; workflow still cannot depend on Fabric. The binary supplies confined
store/fact adapters. Until chairs approve that edge, implement the CLI and
library only; do not invent a generic runtime forwarding layer.

Hook mode reads bounded host JSON from stdin. PreCompact checks the explicit
host-session checkpoint and reports stale/missing data according to the host
protocol. SessionStart emits the bounded rendered checkpoint as additional
context. Host-specific field names and omission/error behaviour have native
fixtures. Never infer another session from the latest file or parse tool-use
transcripts to recover a key. Remove the tracker hook from newly generated
repository setup; preserve other unrelated hooks and user-owned config.

## Interfaces for parallel implementation

These are proposed signatures to freeze, not claims that current crates expose
them. Put shared value/record types in core/records. Runtime services below are
called by the CLI/MCP composition edge; workflow receives values or permitted
confinement ports. This preserves the existing dependency DAG.

| Owner | Required interface and semantics |
|---|---|
| core | `ProjectId`, `DeliveryId`, `SessionId`, `AttemptId`, `RepoId`, `GitOid`, `Digest`, `UtcMicros`, `Deadline`; `GitOid::parse(&str) -> Result<GitOid, IdError>` and `RelPath::parse(&str) -> Result<RelPath, PathError>`. No process-global clocks or language-emulation values. |
| records | Structs in the core-model table; `decode_bounded<T: DeserializeOwned>(&[u8], Limit) -> Result<T, RecordError>`; `encode_canonical<T: Serialize>(&T) -> Result<Vec<u8>, RecordError>`; `digest(&[u8]) -> Digest`. Duplicate keys/range/version failures are explicit; digesting is versioned. |
| config | `workflow_policy(&ResolvedConfig) -> Result<WorkflowPolicy, ConfigError>` and `state_root(&ResolvedConfig) -> StateRoot`; resolved snapshot includes policy digest, limits and explicit overrides, with existing precedence owned by config. |
| supervisor | Safe `SafeFs`, `SafeDir`, `SafeFile` and `PublishMode` through confinement; stable lock inode, no-follow open, no-replace/replace and file/directory sync. `run_utility(UtilitySpec, Deadline, CancelToken) -> Result<UtilityResult, ProcessError>` for fixed git/gh reads at the edge, distinct from provider custody. |
| confinement | `open_workflow_store(&StateRoot, ProjectId, &Grant) -> Result<StoreRoot, ConfinementError>`; `open_artifact(&Workspace, &RelPath, &Grant) -> Result<ReadHandle, ConfinementError>`; `narrow(&Grant, &Grant) -> Result<Grant, AuthorityError>`; `git_facts(&RepoHandle, GitOid) -> Result<GitFacts, GitError>`. Handles retain directory identity; no check-then-reopen path API. `StoreRoot::lock(&RelPath, Deadline) -> Result<LockedRecord, StoreError>`; `LockedRecord::read(Limit) -> Result<Option<Vec<u8>>, StoreError>` and `publish(&[u8], CreateOrReplace) -> Result<(), StoreError>` include sync and typed uncertain publication. |
| run | `completed_attempt(AttemptId) -> Result<AttemptFact, RunError>`: observed terminal state/exit, subject, output digest, route and reviewer identity. `work_snapshot(WorkScope) -> Result<RunWorkSnapshot, RunError>`: generation, observed time, active attempts and held leases. No workflow dispatch/launch/exit copies and no dependency on Fabric tasks. |
| providers | `RouteProvenance` and `ReviewEligibility` records in records; `validate_provenance(&RouteProvenance) -> Result<VerifiedLineage, ProviderError>`. Adapter/endpoint/model families stay distinct; no guessing. Workflow consumes the verified record, not providers directly. |
| routing | `ReviewAvailability { observed_at, valid_until, policy_digest, eligible_tiers, unavailable_families: Vec<AvailabilityReason> }`; `review_availability(&RoutingSnapshot, &ReviewDemand) -> ReviewAvailability`. Include quota/config/capability blockers and family-slot shortfalls with evidence IDs; no fresh probes inside a receipt lock. |
| quota | Quota-derived availability reasons feed routing. **No workflow call or direct dependency**; quota cannot waive review counts or mutate delivery state. |
| telemetry | `upsert_delivery(DeliverySummary) -> Result<(), TelemetryError>`; key `(delivery_id, revision)`, subject, actual route links, acceptance basis, repair count, outcome and explicit missingness. `delivery_report(DeliveryQuery) -> Result<DeliveryReport, TelemetryError>`. Host retries ingestion; telemetry failure never reverses acceptance or cancels a process. |
| cli | `WorkflowArgs` in the shared clap tree; `execute_workflow(args, &HostContext) -> CliOutcome`. Gather bounded fixed-command `GitFacts`, `MergeFact`, attempt and availability facts before calling workflow. Command facts include actual argv/result digest and exact head; failed `gh` reads remain missing evidence. |
| fabric-mcp | Pass cancellation and confined context to the two functions; own schema registration, transport threads, response framing and bounds. Requires the explicit dependency amendment above. |
| fabric-runtime | `work_frontier(WorkScope) -> Result<FrontierSnapshot, RuntimeError>` combines the run snapshot with task-store selectable counts/generation. It is an observed projection, not an atomic global idle claim. Host adapters supply it to workflow without a reverse crate dependency. |

Workflow exports `check_delivery(&DeliveryReceipt, &EvaluationContext)
-> ValidationReport`, `check_session(&SessionCheckpoint, &FrontierSnapshot,
UtcMicros) -> ValidationReport`, `lint(&str, &WritingRules) -> Vec<LintFinding>`
and `apply(&DeliveryReceipt, &Update, &EvaluationContext)
-> Result<DeliveryReceipt, WorkflowError>`. `Store::commit` performs revisioned
publication through `StoreRoot`. `ValidationFacts` contains only referenced,
verified artefact/attempt/merge/approval facts, their source digests and times.
`EvaluationContext<'a>` is a non-serialisable struct containing `now: UtcMicros`,
`actor: &'a VerifiedActorContext`, `policy: &'a WorkflowPolicy` and
`facts: &'a ValidationFacts`. The host supplies actual actor identity and time;
neither is accepted from mutation JSON. The commit contract is
`Store::commit(id: DeliveryId, update: &Update, actor: &VerifiedActorContext,
policy: &WorkflowPolicy, facts: &ValidationFacts, clock: &dyn CommitClock)
-> Result<CommitResult, WorkflowError>`, where `CommitClock::now() -> UtcMicros`.
After acquiring the lock and reading the revision, the store calls the injected
clock and constructs a fresh evaluation context before calling `apply`.
Check grant/approval/availability expiry at that decision time and record it.
No global clock is read by pure policy; tests inject a controlled clock.
`CommitResult { revision, digest, repeated: bool }` identifies the commit.
`AttemptFact` includes `attempt_id`, `terminal: Completed | Failed | Cancelled`,
`exit: Option<i32>`, `subject: Digest`, `output: ArtifactRef`, `route:
RouteProvenance` and `reviewer: ActorId`. `FrontierSnapshot` includes scope,
generation, observed time, `selectable_tasks: u32`, `active_attempts: u32`,
`held_writer_leases: u32` and `complete: bool`; unknown/incomplete never proves
idle. `DeliverySummary` contains only the projection fields listed above;
receipt blobs, prompts and raw reviewer transcripts never enter telemetry.

Native git facts disable replacement objects and inherited alternate Git
directories/configuration; use the shared safe Git owner rather than shell
commands built from request strings. `MergeFact` binds PR head, reviewed tree,
actual merge commit/tree and required exact-head CI result. Squash/rebase
merges need an explicit validated tree relation; equality of a PR number or
branch name is insufficient. Evidence for an earlier subject does not silently
transfer. A pause report certifies only its observed frontier. It saves a
resumable checkpoint and never changes process state. A driver stopping
scheduling uses run admission and wakes on new tasks/material changes; no
workflow API claims to atomically pause both run and Fabric. It cannot turn a
bookkeeping failure or provider silence into a stop reason.

## State reset and useful reuse

Fresh state is mandatory. Leave old `~/.agents`, `.agent-run`, transcripts,
telemetry and installed content untouched during the rewrite. Never import old
RUN.json, session Markdown, mission queues, tracker replay files or UI journals.
Active work resumes by an agent reading old documents and explicitly creating
a new authorised receipt/checkpoint, not by a hidden migration command.

Only Fabric messages/tasks and the telemetry ledger are eligible for a
one-shot import under v7. Workflow needs neither to function. The populated
telemetry ledger has cross-linked usage/cursor/observation/delivery history;
mapping that history to redesigned schemas is not trivial. Recommend **no
telemetry import**: retain the database for historical analysis, start the new
ledger clean and do not infer links from coincident old/new IDs. A byte copy
does not constitute a useful import into a different schema.

Legacy `fabric.sqlite3` has 12,456 messages, 475 tasks, 2,608 agents and seven
task dependencies. Replies have a declared FK; project, sender and ownership
links are mostly soft keys. The separate `fabric-v1.sqlite3` has 228 tables,
two messages and five tasks with run/authority relationships. Importing rows
into a new identity/authority model needs decisions about authors, project
mapping, replies and dependencies. Recommend **no Fabric import** as
non-trivial, with zero implementation lane-days allocated. An archive-only
row copy could be small, but does not restore live task/message semantics.
The store chair may overturn this only with a bounded, trivial projection
that retains these meanings without importing execution state or authority.

| Material inspected | Keep | Strip or move |
|---|---|---|
| Current workflow Rust, 8,771 src lines plus 371 test/fixture lines | Review coverage cases, span/exclusion logic, authority containment cases, bounded reverse-reading lessons and explicit custody refusals | Rebuild around typed models; remove `pending`, Python whitespace/word/dictionary encoders, oracle harness and old CLI parser/rendering. Native cases survive by policy meaning, not old byte output. |
| [PR #1344](https://github.com/mblauberg/provenant/pull/1344), head `8ba2fd9665bef40d433526fdf667b727d3851ce4` | `delivery_run_invariants` risk/closed-write cases; receipt operation preconditions; `context_audit` path escape/freshness cases; style spans and bounded host traits | Do not merge wholesale: cut Herdr checker, tracker shell lexer/queue, bootstrap template engine, reference evaluator, legacy receipt commands and all Python oracles. Move `lane_launch`, `lane_exit`, `provider_observer`, `dispatch_preflight`, `writer_lease` to their run/provider/confinement owners if useful. |
| [PR #1332](https://github.com/mblauberg/provenant/pull/1332), head `f3fbb052cb56ebf3309ae0cfcc672990acd93c2a` | Typed store error/contention handling, transaction tests, `MessageStore`/`TaskStore` seams, injected clock/IDs and lease fencing | Store chair owns adoption. Strip `legacy.rs`, JS coercion/UTF-16/Number emulation and differential/Node harness. Workflow must not use its SQLite store as a receipt database. |
| [PR #1334](https://github.com/mblauberg/provenant/pull/1334), head `c02afccadea65d79474d8b9718dd0bdee57655a5` | Typed retained run facts, identity boundaries, bounded graph analysis and execution facade lessons | Runtime chair owns adoption. Remove Python owner subprocesses, ordered-dictionary/repr emulation, duplicated route defaults/alias maps and oracle helpers. Expose one verified attempt fact to workflow. |

## Rust-native verification

| Layer | Cases and tools | Dependency/ADR change |
|---|---|---|
| Unit policy | `cargo test -p provenant-workflow --lib`: each transition, malformed/tagged records, authority narrowing, risk escalation, stale subject, missing approval, duplicate reviewer, tier/lens/family fallback, failed versus unavailable, exact evaluation binding | None; standard Rust test harness. |
| CLI snapshots | Rust integration process fixtures against the new `provenant workflow` tree; checked-in UTF-8 expected text/JSON, `assert_eq!`, injected clock/IDs and isolated `tempfile` roots | None; no `insta` needed. Freeze new output, including errors/exits and no side effects for help. |
| MCP snapshots | In-memory Rust transport/stdio fixture for initialise/list/call, tagged schemas, malformed input, concurrent requests, cancellation and `isError`; protocol requirements come from MCP | None; use the existing hand-written MCP owner, no Node SDK or replay of old tool output. |
| Store concurrency | Two Rust test processes wait on handshakes, update the same revision, assert one winner and typed conflict; duplicate operation retries; independent project/session locks | None; channels/pipes/barriers, no sleep-based assertions. |
| Fault injection | Inject read/write/rename/fsync/lock failures and ENOSPC through safe filesystem traits; kill Rust fixture at each publication boundary; reread old/new complete receipt or `CommitUncertain` | None; OS effects stay in supervisor tests on Linux/macOS. |
| Artefact/Git facts | Symlink swap, non-regular file, traversal, non-UTF-8 edge, changed digest, replacement objects, hostile Git env, SHA-1/SHA-256, wrong merge/head/tree, failed or stale CI read | None; local fixture repos and a fake fixed-command runner. No live provider/GitHub dependency. |
| Writing/session | Markdown/LaTeX exclusions, Unicode coordinates, overlapping matches, malformed overlays; wrong session, stale/missing checkpoint, busy/unknown frontier, resume triggers, bounded hook context | None; native fixtures with expected policy results. |
| Final product | Install/run with Python and Node absent from PATH; all skill/hook caller instructions use the new binary or external browser tools; source/reference audit; clean-state install on both Macs | Shared install/CI owners; no old implementation as oracle. |

Add deliberate native mutations such as duplicate reviewer counting, accepting
a mismatched subject or dropping the revision check; each relevant Rust test
must fail. This tests the new invariants, not an old implementation.
Full suites, stress and platform conformance run in GitHub Actions. Locally
run targeted changed tests, formatting and static checks. Required CI is tied
to the PR's exact head. No merge or cut-over authority is implied here.

## Deletion inventory and size budget

The inventory below distinguishes workflow-owned deletions from shared final
closure. A path is counted once. A mixed test is removed when its last caller
goes; until then delete only retired assertions and move remaining policy
coverage into the surviving Rust owner's tests. No test is reported as passing
because it was deleted. Counts are baseline physical lines, not estimates of
test coverage or future patch size.

### Workflow source, including telemetry hand-off

| File | Lines |
|---|---:|
| `scripts/telemetry_changes.py` | 406 |
| `scripts/telemetry_delivery.py` | 276 |
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
| `skills/orchestrate/scripts/git_evidence_packet.py` | 88 |
| `skills/session/scripts/cleanup_run_artifacts.py` | 202 |
| `skills/session/scripts/compact_reinject.py` | 169 |
| `skills/session/scripts/context_audit.py` | 305 |
| `skills/session/scripts/state_check.py` | 282 |
| `skills/setup-repo/templates/claude-tracker-hook.py` | 68 |
| `skills/tracker/scripts/steward.py` | 156 |
| `workflows/codebase-polish.js` | 568 |
| `workflows/cross-verify.js` | 455 |
| `workflows/implement-run.js` | 885 |

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

### Shared-owner closure

These files also disappear in the whole rewrite, but their implementation and
deletion belong to the named shared owners. They are excluded from the workflow
size estimate; their callers move within their Rust owner, with no adapters.

### Shared helpers and installer

| File | Lines |
|---|---:|
| `scripts/install-workflows` | 315 |
| `skills/_shared/bounded_process.py` | 230 |
| `skills/_shared/custody.py` | 327 |
| `skills/_shared/excludes.py` | 24 |
| `skills/_shared/harness_profile.py` | 403 |
| `skills/_shared/roots.py` | 45 |
| `skills/_shared/workspace_paths.py` | 78 |
| `skills/orchestrate/scripts/run_dir_finalize.py` | 654 |

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
| Workflow legacy source | 12,214 | 45 files, includes 682 telemetry-owned reducer lines with explicit hand-off. |
| Workflow tests and Python adapters | 7,470 | 26 files; includes 279 lines of Rust crate interpreter support. |
| UI runtimes, wrappers, tests and assets | 30,607 | 69 files; includes generated bundle, vendor, licence and both runtime tests. |
| **Legacy deletion subtotal** | **50,291** | Excludes shared-owner closure and edits to retained files. |
| Existing inert Rust/neutral fixtures to replace or reuse | 8,863 | 8,771 source lines plus 92 neutral fixture lines; excludes the 279 already counted above. No deletion credit: some useful code moves to other crates. |
| New native implementation and tests | about 6,000 | 3,500 production/adapters plus 2,500 Rust-native test/fixture lines, including reused code. |
| **Conservative net reduction** | **about 44,000** | 50,291 - 6,000 = 44,291; existing Rust simplification and shared-owner closure provide additional savings only when measured. |

This is a budget, not a promised diff size. Count files again after integration;
do not inflate savings by counting a generated source or test twice. The new
design document and small replacement skill guidance are outside the runtime
estimate. There is no new UI package or UI Rust module.

## Implementation lanes

Every lane is one to two lane-days including targeted native tests and one
review/repair pass. This is a dependency plan for the redesigned owners,
not slice-by-slice caller switching. Different files can be implemented in
parallel; integrate in dependency order. Old sources go with their Rust owner;
the installer continues to point at the stale install until the final cut-over.

| Lane | Days | Depends on | Deliverable and deletions |
|---|---:|---|---|
| A. Contract freeze | 1 | Both chairs | Freeze shared types, command subtree, root, MCP edge and fact/FS interfaces; remove no runtime files. |
| B. Records and policy | 2 | A, records/config interfaces | New serde receipt/checkpoint, requirement expansion and native constructors; delete reference builders, portable context stamper, old shape/policy registries. |
| C. Review engine | 1.5 | A | One validator and adversarial fixtures; delete ladder/plan/panel/terminal Python, remove Rust compatibility models and route-map copies. Can run with B/D/E. |
| D. Writing | 1 | A | One matcher and overlays; delete three Python linters and their retired wrapper tests. Independent of delivery storage. |
| E. Session and hooks | 1.5 | A, safe FS/frontier records | Typed checkpoint, bounded hooks and pause report; delete six session/autopilot helpers, transcript search and old queue parsers. |
| F. Receipt store | 2 | B, confinement/supervisor safe FS | Lock/revision/idempotency/publication/fault cases; delete producer command/process modules and scattered write helpers. |
| G. Delivery and merge evidence | 2 | B/C/F, verified run/Git facts | Acceptance/release/closure, evidence/evaluation binding and merged continuity; delete validator cone, binder/checkpoint scripts and corresponding tests. |
| H. UI and accelerator retirement | 1.5 | A; council decisions accepted | Delete all enumerated UI source/bundles/tests, three JS workflows, tracker hook/steward and Herdr checker; update skill/tool instructions and ask config owner to remove installer registrations. Parallel to B to G. |
| I. CLI/MCP and telemetry edges | 2 | C/D/E/G, shared tree freeze | Wire typed requests, new snapshots, hook protocol fixtures and delivery summaries; telemetry owner deletes old delivery/changes readers. |
| J. Closure and install acceptance | 1.5 | H/I, whole-product owners ready | Remove interpreter oracles, shared dead helpers, Node manifests/lockfile and CI provisioning after last consumer; run native end-to-end gates and one cut-over install. |
| **Total** | **16.5** | Shared owner work excluded | Working estimate; storage/process API delays do not justify new compatibility layers. |

## Decisions for the chairs

| Decision | Recommendation | Owner and consequence |
|---|---|---|
| Bespoke UI runtime | Retire every component; replace browser evidence with existing external tooling | Both chairs/user council. This deliberately removes live overlay acceptance and custom detector scores. |
| JS workflow host | Retire all three accelerators; skills remain the workflow | Config/host chair. No JS bridge, Rust interpreter or `meta/run` replacement contract. |
| CLI subtree and machine exits | Adopt the proposed `workflow` subtree and common typed updates | Shared command-tree freeze; final spellings are not settled by this document. |
| MCP dependency edge | Permit direct `fabric-mcp -> workflow` with binary-supplied ports | Both chairs amend layering. It avoids a subprocess and a forwarding API in fabric-runtime. |
| State root | New `provenant-rust` root, never old state directories | Config chair freezes spelling and identity mapping. No workflow migration. |
| Ledger import | Decline telemetry import as non-trivial; preserve the old database | Telemetry chair/user can retain it for analysis without bringing the old schema into the new binary. |
| Fabric import | Decline as non-trivial unless the store chair proves a small semantics-preserving projection | Preserve both stores; no authority, run/session or claim revival and no workflow dependency. |
| Requirement presets | Keep five names as compact data; remove generic trajectory obligations and nested overlay execution | Workflow/config chairs. Preserve actual user/scoped criteria and evaluation/security/qualified-review requirements. |
| Hook behaviour | Keep compaction restore with explicit session identity; retire tracker denial hook | Config chair owns managed config reconciliation. User-owned hooks remain untouched. |
| Extra crates | None | Standard test harness and current allowlist suffice; no parser/browser/HTTP runtime should be added to save retiring UI. |
| Existing inert Rust | Salvage policy cases and narrow algorithms; remove compatibility scaffolding | Each crate owner. Merged Rust is not evidence that its old internal model should survive. |
