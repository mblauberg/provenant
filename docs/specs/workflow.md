# Workflow

Workflow is a small synchronous policy library with a minimal delivery receipt,
a pure review coverage check, bounded Markdown session checkpoints, a data-driven
writing matcher and a session handoff between hosts. For pull request work the
pull request carries the evidence. Skills conduct the work with `git`, `gh`,
Fabric and browser tools.

The [cross-cutting decisions](README.md#cross-cutting-decisions) govern state,
layout, commands, waits, records and hooks, and the
[workspace rules](rust-workspace.md) govern dependencies, errors and unsafe code.
This document grants no merge, install or release authority.

## Purpose and ownership

`tools/front-door/crates/workflow` is `#![forbid(unsafe_code)]`. Its internal
dependencies are core, records and the OS crate's concrete filesystem API (the
[workspace table](rust-workspace.md#crate-layering) lists the allowed maximum),
plus serde, serde_json, thiserror and regex. It holds no SQLite, process
runner, scheduler or MCP tool.

| Module | Responsibility |
|---|---|
| `review`, `coverage`, `risk` | Pure independence, tier, lens and family coverage on an exact subject; risk derivation |
| `receipt`, `store`, `persistence` | The minimal receipt and its bounded read, expected-revision write and atomic replacement |
| `session`, `chair` | Markdown checkpoints, hook context and the chair-session helpers |
| `writing` | Span exclusion and data-driven matching |

| Caller | Need |
|---|---|
| Agents following skills | Record evidence and learn what blocks acceptance |
| Chair conducting review | Count independent reviewers with adequate tier, lens and family coverage |
| User at the CLI | Inspect a receipt, understand a refusal, save and resume a checkpoint, hand a session to another host |
| Harness hooks | Restore the right session after compaction |
| Writing skills | Locate mechanical prose defects under general, academic and legal rules |
| Telemetry | Join a delivery to its terminal run records through `DeliveryId` |

## Boundaries

- **Records** owns the receipt, review, provenance and policy wire types.
  Workflow owns their semantic checks.
- **Confinement** owns grants and authority narrowing, and supplies held
  directory handles and validated ID components. Workflow receives a confined
  actor context and records the approval references it needs.
- **Run** owns attempts and the `AttemptFact` that review binds to. Run's
  terminal records carry the `DeliveryId`, and `telemetry stats` reduces them
  with the price table. There is no telemetry ledger and workflow offers no
  report API.
- **Providers and routing** own verified model-family lineage and review
  availability. Workflow never guesses a family from a name and never depends on
  run, routing or providers. Fact types implement `Serialize` only, and the
  composition edge builds them from owner reads. Request JSON therefore cannot
  decode into a verified fact. This is a code boundary, not a cross-crate
  unforgeable guarantee.
- **Skills** own their domain obligations. Evaluate, release and the domain
  skills keep their own evidence rules, outside the minimal receipt.
- **Fabric** owns peers and the session handoff transport.
- **CLI** owns the envelope, the verb tree and the hook table. Workflow adds no
  MCP tool and no `fabric-mcp` edge.

## Contract

### Receipt and review

| Type | Contents |
|---|---|
| `Subject` | `Pr { repository, number, head }` or `Document { path, digest }`. Reviews bind this exact subject. |
| `DeliveryReceipt` | Version, delivery and project IDs, revision, subject, authors, risk, review attempt references, decision, created and updated times |
| `Author` | Actor ID, user or agent kind, route provenance including actual tier for an agent |
| `Decision` | Pending, accepted or abandoned, with actor, time, reason and any originating user approval. No release or close states. |
| `ReviewFact` | Attempt and reviewer IDs, subject, output reference and digest, verdict, blocking findings, route provenance, named lens |

For PR work the PR is the evidence: exact-head required CI, review findings and
repairs, then merge through the repository workflow. A receipt records the
chair's decision and is not a second merge certificate. A changed subject needs
fresh review and a new pending decision. IDs and creation time are immutable,
and each save rechecks derived risk and requires an originating approval for a
downgrade. Risk downgrade and terminal or scope-required acceptance keep their
explicit user approval gates. Prose in a checkpoint cannot expand a grant.

A review passes only for a retained matching verdict and output on the exact
subject, with adequate tier and independent reviewer identity. Authors and
decision makers, including the chair, never count as reviewers, and repeating a
reviewer adds nothing. `approve_with_nits` passes only when every remaining
finding is advisory. Failed work or a missing verdict is not provider
unavailability.

| Provenance | Author tier | Same-family review | Cross-family review |
|---|---|---|---|
| `Observed` | Counts when verified from the attempt or host record | Counts | Counts only with providers-verified family lineage |
| `ChairAttested` | Counts, with the native host parameter, tier and evidence reference recorded | Counts, with an independent actor and lens | Never counts |
| `Unknown` | Blocks substantial-or-higher coverage | Findings only | Never counts |

Chair attestation never promotes itself to observation. A cross-family leg
needs an observed, verified reviewer route separated from every author family,
and an unknown author family leaves separation unproven. Adapter, endpoint
provider, model family and review eligibility stay separate fields.

| Risk | Counted independent passing reviews | Coverage |
|---|---:|---|
| Routine | 0 | Chair plus objective checks |
| Substantial | 1 | Other family if available; otherwise a recorded blocker and a same-family reviewer through a distinct lens |
| Crucial | 2 | Distinct lenses and two other families if available; each unavailable leg has a recorded blocker and an independent same-family replacement |
| Terminal | 2 | Crucial coverage plus an adversarial lens, which may be one of the two; explicit user acceptance |

Every counted reviewer meets the highest author tier. Family separation is
measured against all authors, not the chair's family. An availability blocker
needs source references and an expiry, and a reason string alone never waives a
leg. Distinct lenses and the independent count survive fallback.

The binary binds review facts at the CLI edge from run's `AttemptFact` and a
chair `ReviewClaim` (lens, verdict, findings, tier), because routing publishes
no tiers. The reviewer identity is the dispatched attempt's own execution
identity, never the submitting chair. The family comes from the routing
catalogue entry for the observed model, and a model missing from the catalogue
stays unknown. A PR subject needs the attempt head to equal the subject head. A
read-only lane's head is the checkout HEAD the host read at dispatch, never lane
output. `LegBlocker` and `user_approval` are chair-supplied references that the
CLI records and does not verify. Accepting needs an existing receipt at revision
1 or later and runs coverage, refusing with `REVIEW_INCOMPLETE` unless it
passes. Coverage uses the compiled `ReviewPolicy` and `RiskPolicy` defaults.

### Persistence

Workflow state lives under the host-global state root
(`~/.local/share/provenant/v1`, see the cross-cutting decisions) at
`workflow/projects/<ProjectId>/`:

```text
deliveries/<delivery-id>/receipt.json
deliveries/<delivery-id>/receipt.lock
sessions/<host-session-id>/checkpoint.md
sessions/<host-session-id>/checkpoint.lock
```

No path comes from a directory scan, a lossy conversion or a `..` component.
Records performs bounded decoding and canonical encoding (sorted keys, UTF-8,
integer UTC microseconds, no final LF in the canonical bytes; files may append
one LF and digests cover the canonical bytes). Duplicate keys, unknown versions
and tags, oversized input and invalid integers are refused. Git OIDs accept
SHA-1 and SHA-256. Limits: receipt 64 KiB with 256 review references,
checkpoint 32 KiB, hook input 1 MiB, hook output 8 KiB.

A save replaces the whole receipt at an expected revision. It takes a stable
lock inode, rechecks the revision and subject, validates the replacement,
writes and syncs a same-directory temporary file, renames it over the receipt,
syncs the directory and increments the revision once. Locks are never unlinked
or recreated. There is no blob store, event log or operation history. A crash
leaves a complete old or new file, and a failed sync after the rename reports
the failure and tells the caller to reread rather than claim a rollback. A
corrupt stored receipt fails closed with no repair path, and a leftover
temporary file is ignored by the next save.

Lock acquisition polls a non-blocking `flock` against a monotonic deadline
through the OS crate's concrete filesystem API. No lock is held across routing
probes, provider waits or GitHub reads: gather facts first, then recheck the
subject and expiry under the lock. Time is read after the lock is taken, and
`updated_at` is the later of the previous value and now. Workflow starts no
thread or subprocess and never cancels another owner's attempt or lease.

### CLI and hooks

The [CLI](cli.md) owns the envelope and the verb tree. Workflow's leaves are
(run `provenant help workflow` for the flags):

```text
provenant workflow review --request <file|->
provenant workflow receipt show <id>
provenant workflow receipt save <id> --expected-revision <n> --request <file|->
provenant workflow session save <session-id> --file <markdown|->
provenant workflow session show <session-id>
provenant workflow session handoff --to <peer> --account <label> [options]
provenant hook claude pre-compact|compact
provenant check writing <paths...> [--rules general|academic|legal]
```

`review` is the pure coverage check. A new receipt uses expected revision zero.
`save` takes trusted actor context from the host, never from JSON that claims
authority. `show` reports stored claims without asserting current verification.

Hooks are rows in cli's `HookSpec` table with host-native stdout and exit
contracts rather than the CLI envelope. They are advisory: missing or invalid
state exits 0 with a bounded diagnostic that names a cause kind and never a
path. SessionStart takes the actual session ID from the host event and injects
that validated key with the save command
`provenant workflow session save <session-id> --file <checkpoint.md>`, then only
that key's checkpoint or an explicit missing or stale status. A checkpoint is
stale when the file's modification time exceeds the caller-supplied maximum age
or lies in the future, and stale content is withheld. If the encoded output
would pass 8 KiB, it emits an excerpt marker and a `workflow session show`
remedy. PreCompact reports whether the checkpoint needs saving and never
searches transcripts. A missing or invalid host ID yields a diagnostic and no
checkpoint, never a fallback to the latest file. Host session IDs are 1 to 128
ASCII letters, digits, periods, underscores and hyphens, starting
alphanumeric, with no `..`, so the injected command is safe to copy.

A checkpoint is ordinary Markdown: goal, authority and evidence references,
progress, next actions, blockers and resume triggers. It is bounded UTF-8 under
an explicit host and project key, with no shape validator or JSON wrapper. A
save writes atomically under the session lock and preserves bytes, including
line endings.

### Session handoff

`workflow session handoff` hands a chair or task session to a fresh Claude
session on a peer Fabric host. The binary composes it, because it needs the
worktree and peer owners. The peer gets a generated prompt rather than the
transcript: the checkpoint, each branch with its pushed commit, the lanes still
running on the source and any notes.

- The source checks its worktrees. They must be clean and pushed. `--wip`
  commits tracked changes and pushes them first, `--allow-dirty` proceeds and
  lists them in the prompt, and `--dry-run` prints the prompt and plan and
  sends nothing. The checkpoint is saved and the owner lease moves only after
  the peer accepts.
- One `session_handoff` peer action carries the prompt, attachments and
  branches. The prompt is at most 32 KiB, with the checkpoint cut to fit, and
  at most 32 attachments totalling 512 KiB. `--prompt-file` replaces the
  generated body, and `--note` adds lines.
- The target checks `--account` against its own registrations, recreates each
  branch's worktree from `origin/<branch>` with its own worktree owner (a
  worktree already on the branch is kept, and failures are reported per branch
  without failing the handoff), stores the prompt and opens Terminal running
  `claude` under that account. `--no-launch` delivers without opening Terminal.
  Only `claude` is a supported provider.
- A repeated operation ID is a replay: it never launches again. Each host keeps
  an `owner.json` under `handoff/<ProjectId>/` naming the host that owns the
  session, and both flip it on a delivered handoff.

### Writing checks

`check writing PATHS` loads bounded JSON rules from `lint-rules.json` beside
`skills/natural-writing` and `skills/legal-writing`; the general and academic
rule sets share natural-writing's file. Rules compile once, exclude code
fences, inline code and declared LaTeX command and math spans, and report
deterministic rule IDs with Unicode line and column and byte ranges. Findings
exit 1. Unsupported patterns and oversized rule sets are refused. Legal
checklist failures remain filing blockers in that skill. Writing rules enter
neither config nor records.

### Route and topology evidence boundary

Dispatch and batch receipts own the actual resolved route, provider, model and
attempt facts. Delivery may reference those receipts as evidence. Fabric MCP may
start the existing dispatch owners, but it implements no provider mechanics and
owns no lifecycle acceptance. Assurance labels reflect the evidence that exists
and infer no coverage. Nothing here defines a learned router, a global
leaderboard, automatic promotion or a universal digest or version protocol.
Route or topology comparison is task-local and opt-in under the evaluation
triggers, and the owning dispatch or batch receipt remains the source for what
actually ran. An issue or receipt records scope or evidence and is not approval:
release, promotion, activation, sharing, provider login, live installation,
push and destructive action stay gated by current user authority.

## External contracts

- **Harness hooks.** Claude Code's `PreCompact` and `SessionStart` stdin and
  stdout shapes, with native fixtures for field names and omission behaviour.
  PreCompact's `systemMessage` is advisory and Claude discards it, so the CLI
  may also print the diagnostic to stderr. Neither event emits a blocking
  decision.
- **Agent Skills.** `SKILL.md` frontmatter, and the Workflow host in Claude
  Code (`meta` and `run` exports), which Provenant neither replaces nor owns.
- **Git and GitHub.** Pull request heads and CI come from `git` and `gh` reads
  through confinement's fixed utility facade.

## Failure and recovery semantics

- Errors cover malformed input, version, limit, stale subject, authority denied,
  conflict, contention, cancellation, deadline and I/O, with redacted paths and
  causes. Reports are deterministically ordered.
- Cancellation before publication has no committed effect. Once the final
  cancellation check before the atomic write passes, the bounded stage, rename
  and sync finish regardless of later cancellation.
- Two saves at the same revision give one winner and one conflict. Delivery and
  session locks are independent.
- A handoff that fails before the peer accepts leaves the checkpoint and owner
  lease unchanged, so it can be retried.
- Active work resumes by an agent reading earlier documents and saving a new
  receipt or checkpoint. No command migrates old state.

## Deliberate cuts

| Cut | Reason |
|---|---|
| Panel presets, axis overrides, membership and reducer registry | Skills request lenses and synthesise findings; panel metadata adds no assurance beyond individual reviews |
| Provider-family maps in workflow | Providers and routing own verified lineage |
| Portable-workflow context stamps and per-skill wrapper schemas | A completed stamp proves no outcome; use the PR or the skill's document |
| Five delivery profiles, overlays and policy callback dictionaries | Skills state criteria directly |
| Evaluation bindings, security evidence, outcome and trajectory measures | Evaluate, release and domain skills own their evidence; no universal metric records |
| Receipt mutation commands, repair and finding sidecars | A bounded replace at an expected revision; findings and repairs belong in PRs |
| An evidence subprocess runner | Execute through run or an authorised command and keep the result in the PR; no second custody owner |
| Reference-run builders and synthetic passing records | Native test constructors cover them; they are not a product feature |
| Merged-delivery binder and implementation checkpoints | The repository workflow owns exact-head CI and merge; session Markdown carries progress |
| Session shape checks, audit and transcript reinjection | An explicit host session key replaces transcript searching |
| Scratch cleanup from the session helper | Retention belongs to the run guardian; no workflow deletion command |
| Mission scaffolder, knob parser and Markdown item leases | Fabric tasks and run admission own work and leases |
| Idle-frontier pause validator | Autopilot reads `lanes` and Fabric tasks directly; run owns admission |
| Tracker replay queue, outcome writer and shell-parsing hook | A file write cannot make a GitHub side effect exactly once, and shell recognition is incomplete; reconcile issues before retrying |
| Herdr CLI contract checker | Workflow no longer invokes Herdr; the integration owner tests any retained transport |
| JavaScript implement-run, cross-verify and codebase-polish engines | Skills and Fabric already conduct these sequences; no generic workflow interpreter |
| A delivery telemetry ledger and cost-per-accepted-outcome ingestion | `telemetry stats` reduces terminal run records with prices; the receipt plus run records are the record |
| UI detector, live server, overlays, annotation and variant accept | Read-only `ui-ux-design` review plus the optional external Playwright skill and the app's own dev server replace them; the Rust binary needs no DOM parser, browser server or UI tool |
| Workflow MCP tools | CLI `--json` suffices |

## Verification

| Where | What it proves |
|---|---|
| `crates/workflow/src` tests (`coverage`, `review`, `receipt`, `store`, `session`, `writing`) and `crates/workflow/tests/fixtures` | Duplicate actors, author and decision-maker exclusion, tier, lens and family fallback, `Unknown`, `ChairAttested` and `Observed` provenance, terminal adversarial counting, stale subject, required approval; bounds, version and revision; concurrent saves with a single winner; symlink and traversal refusal, SHA-1 and SHA-256, wrong PR head; writing exclusions, Unicode coordinates and malformed or oversized rules; hook decoding and the SessionStart cases |
| `tools/front-door/tests/workflow.rs` | The envelope and exit snapshots with isolated roots, side-effect-free help, and the receipt, review and session flows |
| `tools/front-door/tests/hooks.rs` | Native hook stdin and stdout behaviour |
| Tests in `tools/front-door/src/handoff.rs` | The handoff plan, replay and refusal paths against a recording launcher, so no test opens Terminal or starts `claude` |
