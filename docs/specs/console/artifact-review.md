# Project Fabric Console artifact review and user attention

## Scoping and artifact review

Extended scoping and grilling occur in the interactive Claude or Codex chair
pane, not in a second chat implementation inside the Console. The Console shall
open or focus that pane and display `scoping`, open decisions and artifact
status.

The chair shall persist decisions into project artifacts. Pane scrollback is
not authoritative. The Console shall provide an artifact view containing:

- rendered spec and ADR files;
- the diff since the previous review;
- open decisions and review findings;
- exact paths, revisions and digests;
- `Discuss`, `Accept`, `Request changes`, `Defer` and `Implement...` actions.

Artifact bytes are read only through negotiated
`artifact-content-read.v1`. The Console requests pages of at most 131,072 UTF-8
bytes and 2,000 lines, follows only daemon-issued monotonic cursors and can
continue until `nextCursor` is null. It never opens a project or private path
directly. Feature absence, unsupported/unsafe content and unavailable pages
leave the metadata view usable but visibly incomplete.

Every content pane displays, adjacent to the bytes, the exact source path and
digest, evidence revision, publisher provenance, evidence kind, current page/
coverage, source and complete-rendering sizes, complete-rendering digest and
the transformation label. The source digest is explicitly labelled as
certifying the immutable source, not displayed bytes, whenever transformation
is not `none`. Each returned page digest is verified before display; after all
pages, their ordered bytes must reproduce the complete-rendering digest.
Missing, duplicate, skipped, reordered, stale or cross-artifact cursors discard
the local review coverage and require a fresh detail/read sequence.

`Accept` and `Implement...` remain disabled while any content page is missing,
unverified, stale, unsafe or unavailable. A terminal-neutralised complete view
requires a distinct explicit confirmation that names the transformation and
source digest. A capability- or credential-redacted view cannot be accepted or
implemented from the Console because material source bytes are hidden;
`Discuss` and `Request changes` remain available to obtain a clean replacement.
The confirmation preview records complete page coverage and the verified
source/rendered digests but the reads themselves create no acknowledgement or
authority. Resize, detach and restart preserve only cursor/review UI state that
still matches the exact evidence revision; they never convert it into approval.

Natural-language acceptance in the active chair conversation may satisfy a
gate only when a contract-tested provider/Herdr integration identifies it as
direct human input and binds it to the operator principal, expected revision,
exact gate and artifact digests. Echoed text, agent-authored text, unavailable
direct-input provenance, raw pane scraping or CLI/pane-injected text cannot
approve. The integration must attest the operator input channel independently
of terminal content. The adapter shall record the provider message ID, exact
human utterance, artifact digests and interpreted decision. Ambiguity shall
trigger clarification and shall not silently approve. One-way, destructive,
external-effect, release and final-acceptance decisions require an interpreted
decision preview showing the gate, revision, digests and consequence plus an
explicit confirmation; low-consequence decisions may commit directly when the
gate reference is explicit. The user may always use the typed Console action.

After acceptance, `Implement...` shall prepare an editable launch packet. It
may target the current chair for minor work, a fresh lead/provider context under
the coordinated chair, a handoff-based chair replacement, or an independent
run chair. The prompt shall reference artifact paths and digests and require the
receiving owner to reopen them. It shall not paste large artifacts into the
prompt.

Fresh external implementation review is a Fabric review task, not an
unattributed provider transcript. Through the authenticated current chair,
Fabric seals `delivery-requirement-map.v1`,
`coordination-gate-snapshot.v1` and
`implementation-delivery-manifest.v1`, then creates the review task. Callers
cannot supply their bytes, Git base or evidence list. AFAB-004's immutable
approved run-start base is
`c2fc623a2529f87feca27982e1a140969ab5a258`. Fabric proves the publication-time
principal/bridge/provider-custody lineage; an operator-, Fabric-, project- or
Git-published root remains honestly unproved and cannot satisfy this profile.
Git diffs and other evidence remain valid covered inputs.

Requirement-map seal is current-chair-only and requires expected generation
zero iff none exists, otherwise the exact current generation. It hashes a
generation-free catalogue/scope/source/requirement/evidence closure before
allocation: an equal closure reuses the current map, and only a changed closure
allocates current plus one. Command-ID churn cannot stale a completed basis.

The seal derives the exhaustive profile/catalogue requirement/evidence closure,
pre-review gate snapshot and clean canonical Git base/head state, then commits
the immutable review basis. `review-target.prepare` accepts only that current
sealed basis, with expected target generation 0 when none exists and the exact
positive current generation otherwise. It performs bounded DB-only admission,
reserves never-reused target/bundle generations and promptly returns one
immutable accepted preparation receipt; it does not build the bundle inside the
30-second public request. The Console polls only
`review-target-preparation.read` and renders the durable states as Preparing,
Building, Committing, Succeeded, Conflicted or Failed. It shows no percentage or
ETA. `phase-only` shows only the label. `finite` shows exactly
`n/m verified build items` from the immutable plan and never derives another
unit. Terminal detail uses only the closed conflict causes (target, chair,
task/authority, delivery basis, source, profile, predecessor head/action) or
failure causes (bundle size, unsupported repository state, source
   read, content integrity, certifying capability unavailable).

The daemon worker derives the complete changed-file and required-evidence set
and builds one bounded `review-bundle.v1`. Exact before/after/diff/evidence
objects are content-addressed and chunked; a coverage digest proves the complete
manifest. Changed files use the checked-in `review-diff.v1` codec with exact
status/mode/binary/rename/path/order/digest semantics and immutable full-base/
head conformance fixture. The final target dynamically computes the actual
approved run-start-to-sealed-HEAD counts/bytes; a prior delivery-HEAD measurement
is not a gate. The request accepts no summary or caller-selected file list.
Omission, truncation, dirty or changed source/evidence fails. Recovery resumes
the same preparation/generations and may expose only one complete target or a
terminal conflict.

Source, delivery, unrebindable chair or adapter-contract/profile advance makes
the current target stale without mutation; only successful preparation Phase B
persists it superseded while inserting a successor. At true-chair lifecycle
adoption Fabric captures the exact run terminal-sequence certification cut and
either appends a contiguous same-agent/same-subject chair binding or leaves the
target stale. Lifecycle never waits, rolls back or gates on review actions.
The daemon and Console use the exact idempotent `review-target.rebind` receipt;
an already-applied automatic adoption returns that same custody-keyed receipt,
and the Console never supplies family/profile/source/bundle/head claims.
Already-current evidence at/before the first successor cut remains certifying
through the digest-valid chain; post-cut old-binding output is permanently
noncertifying but settles and retains adverse findings. Preparation may wait at
Committing for existing action ambiguity; it cannot leapfrog recovery.

Bundle bytes stay behind the action-pair-only digest-bound portal, so the fixed
rubric/target/profile envelope fits the prompt limit. Every portal read names
the exact bundle, root/page, object and chunk digest. Mandatory consumption is
the complete manifest/read map, delivery manifest/map, required scope/spec/ADR/
decision/gate records and the content-addressed finding-set root plus every page
containing every full carried finding. No fixed finding-count cap or truncation
exists.
A daemon-owned immutable bundle search/risk map then
selects exact highest-risk diff chunks from every nonempty group by checked-in
score/path rules; at most 32 chunks/2 MiB are mandatory.
Zero/insufficient reads cannot certify. Literal bundle search and all other
diff/before/after objects remain available for deeper review.
`certifying-review-packet-only.v1`
binds each activated adapter contract. A daemon-built per-action 0700 synthetic
HOME may contain only exact 0600 adapter auth/config bytes outside the model
read/tool namespace; the model sees only the exact portal. Claude SDK/Codex app-
server may use direct dynamic tools only with identical schema, ledger, source-
denial and cleanup canaries; Codex must prove confinement independently. For
Cursor/Agy, the outer daemon launches the contract-pinned std-only Rust
`agent-fabric-review-portal-supervisor`, persists stopped-child PID/start/PGID/
session/executable custody before exec, and gives the supervisor private control
FD 3 with `CLOEXEC` before provider exec. The provider MCP manager launches that
same binary in fixed `portal-stdio-v1` mode with exactly the non-secret action-
socket/action-pair/contract locators. No inherited provider descriptor or
bearer is used. A one-use AF_UNIX broker verifies local peer credential/PID,
UID, start time, PGID/session, ancestry and exact executable identity. Their
exact model allowlist is
`mcp(agent-fabric-review-bundle/review_bundle_read)` and
`mcp(agent-fabric-review-bundle/review_bundle_search)`; every other MCP,
command, filesystem, web/network, resource and prompt effect is denied.
Discovery returns that one server/two tools; list probes for resources/templates/
prompts return empty. Any extra surface, successful denied effect or outside-
portal/auth-file read invalidates the action.
Portal requests are exactly one UTF-8 JSON object plus LF with no batch,
duplicate key or trailing bytes. ID is integer `0..2147483647` or a 1..64-byte
ASCII `[A-Za-z0-9._:-]` string; response is exact JCS plus LF. Read payloads use
RFC 4648 padded base64 and at most 65,536 raw bytes. Generated templates, not a
prose metadata allowance, prove the complete response is at most 98,304 bytes.
Preparation reserves the maximum 64-byte-ID form; ledgers debit exact canonical
wire bytes for the actual ID. Direct tools receive the same equivalent charge.
Search retains its separate 65,536-byte response limit.
Missing enforcement advertises false, fails before provider I/O and never
falls back. Control EOF/HUP, deadline, cancellation, provider or supervisor
death triggers TERM, 250-ms wait, KILL, reap and socket/capsule removal. Startup
verifies PID plus start time before signalling and treats mismatch as integrity
failure. Recovery also no-follow verifies the private custody directory path/
device/inode and relative socket/capsule names/digests before removal. Canonical
crash-custody metadata never appears in Console, model input or receipts; only
the three minimum non-secret broker/action/contract bootstrap locators enter the
isolated helper environment and confer no authority. A passing canary proves daemon/supervisor crash cleanup and that no
pinned child can escape via `setsid`, double-fork, daemonisation or reparenting.
The model has no executable
tool. Seatbelt/`sandbox-exec` is usable only under an exact-OS-version passing
canary. Agy requires proved direct execution or one exact pinned fixed-argv
trampoline if its hook transits `/bin/sh`. A provider process that also exposes
model web tools needs provider-native separation proof or a destination-
constrained API proxy. The narrow hardened shim is a `std`-only Rust opaque
relay for bounded binary framing/FD/process/peer checks; TypeScript owns JSON-
RPC/MCP/hook semantics and journalling. Stock Cursor/Agy routes remain
capability=false until the pinned supervisor/helper and outer confinement pass every
current-build canary.

One target has one logical bundle/root: up to 64 MiB unique objects, 16 MiB per
object, 16,384 objects and 32,768 deterministic chunks, never a bundle chain.
The mandatory set is at most 80 unique responses/6 MiB. Direct primaries reserve
up to 112 portal calls plus 16 planning/final turns; helper routes reserve 128
internal calls. Their guaranteed exploration headroom is respectively 32 or 48
calls/4 MiB, with a 10 MiB combined wire-byte ceiling; search consumes it and
is capped at 16 calls/1 MiB aggregate.

The target owns an immutable review subject plus one append-only active chair-
binding chain. Generation one snapshots the exact chair agent/principal/lease/
provider-session/bridge generation, adapter/contract/family/model and the
resolved checked-in `certifying-review-four-slot-v1` profile. Its slots are `native`,
`other-primary`, `cursor-grok` and `agy-gemini`; for an OpenAI chair they
resolve respectively to the native Codex route, Claude/Anthropic,
cursor-agent/`cursor-grok-4.5-high`/xAI and
Agy/`Gemini 3.1 Pro (High)`/Google. Native is explicitly exempt from
the reviewer-family relation; each external slot must differ from the target-chair
family. Publisher eligibility separately proves the root publisher equals that
target family, so no duplicate publisher-independence flag exists. Any chair
change outside the exact same-agent lifecycle-binding rule makes the target
stale; only a successful successor preparation supersedes it.

Each resolved slot displays requested effort plus the tagged result:
`applied(<value>)` or `inapplicable`. Inapplicable always has a null request.
The Console never fabricates a sentinel or infers effort from a model label.

Turns, mandatory/exploration reads and exact response bytes are separately
reserved/debited before provider I/O. Mandatory unique reads cannot be spent by
search/duplicates. An adapter or delivery that cannot meet
the mandatory/risk predicate is unavailable, not silently one-shot or falsely
byte-complete.

Before router I/O, a normal review action reserves space for all 32 possible
new safe findings and resulting paged set roots. Failure shows target-wide
`finding-capacity-exhausted`, creates no action/budget and raises the genuine
private-store capacity gate when minimum recording space is exhausted. While
blocked, the Console may offer only an explicitly labelled resolution-only
window over at most 32 existing digests: it admits zero new findings, cannot
certify completion and requires a later normal CLEAN. It never offers truncation
or deletion of a referenced finding.

The Evidence header projects required-slot capability independently of target
existence. If a slot is unavailable it shows top-level
`certifying-review-capability-unavailable` and a profile-ordered
`unavailableSlots[]` row with adapter, contract, family, model and exact safe
reason plus profile/schema, chair family, source/runtime/platform identity and
availability revision. It does not reduce this to `missing-target`. Preparation is disabled
with the same reason before any action, budget or provider I/O.

Only the current target chair at the active binding may dispatch a new
certifying action. Every UI, protocol, recovery and receipt reference displays
the canonical `(adapterId, actionId)` pair; action ID alone is never treated as
identity. The shared
`model-route.v1` codec validates structural routing data only; effort
applicability, artifact/target currency and exact adapter/family/model/profile
agreement are rechecked transactionally after the bounded side-effect-free
router, and resolved adapter must equal the requested action adapter and slot.
Pre-router single-flight is global by provider action pair and binds run plus
authenticated actor/principal/full input: cross-run reuse conflicts before a
second router, while another adapter may reuse the same action ID. One per-
target/slot action-attempt CAS prevents concurrent actions from
running against one evidence head.

Terminal review reads expose safe CLEAN/FINDINGS/UNUSABLE result digests only.
A proved terminal failure has exactly one of `max-turns-exhausted`, `provider-
rejected`, `terminal-no-answer` or `adapter-terminal-failure`; it is distinct
from effect ambiguity and from the broader routing/substitution catalogue.
Every proved-effect safe/UNUSABLE/failure terminal
settles complete authenticated usage exactly or conservatively consumes the
remaining reservation when usage is absent/partial; proved no-effect releases,
while true ambiguity retains. It exposes only the closed result/failure digest
and is never redispatched. Raw answers, errors, bundle objects and final prompts
never enter the Console.

Provider terminal failure closes only its attempt. It creates no evidence and
does not advance the evidence head; its receipt/slot row shows the exact four-
code failure, stable terminal sequence/result digest and explicitly unchanged
head/open/repair set. Safe/UNUSABLE evidence rows show immutable certification
basis at terminal separately from the live current basis: active binding,
predecessor cut/chain or noncertifying post-cut. The UI never
labels a post-cut terminal current merely because a later binding is active.

The other terminal noncertifying states are proved no-effect, route-integrity
terminal and retired-unknown. Permanent provider-effect ambiguity remains
ambiguous with its reservation held until an exact consequential
provider-route-integrity-retire Preview/Commit receives direct-human
confirmation. Retirement performs no provider call, charges the full remaining
ceiling and exposes retired-unknown; it never fabricates no-effect. This is a
genuine review-and-liveness gate: while open it blocks slot dispatch, target
reprepare, review/run acceptance or close, and the budget. Retirement or
proved terminal reconciliation is the only escape from unprovable effect.

Every CLEAN, FINDINGS or UNUSABLE provider terminal transaction automatically
inserts evidence and linearly CAS-advances one per-target/slot head before the
result is visible. There is no chair-skippable terminal result. It names the
prior head, separately reports provider-reported and daemon-accepted resolution
sets, and carries complete prior/new open sets. Insufficient-read CLEAN becomes
UNUSABLE and resolves nothing. Safely parsed insufficient-read FINDINGS stays
visible/noncertifying, resolves nothing and adds every safe finding. An in-
flight answer whose target becomes stale still settles and advances its
reserved head; it resolves nothing, is stale/noncertifying and carries safe new
findings into the successor target. Concurrent attempts
conflict; a second FINDINGS action advances from the returned head. Every safe
P0-P2 is automatically repair-required; no chair annotation can downgrade it.
A repaired target carries full safe ID/severity/summary/evidence plus origin
action/result/manifest/basis/bundle and exact repair currency in mandatory
finding pages. Every repair requires later manifest/basis/bundle; source/mixed
requires Git source advance and evidence/mixed requires each named registration
to advance revision and content. Git-only change cannot repair evidence-only.
Only then may CLEAN resolve the digests.
Evidence detail resolves those safe records only through scoped
`review-finding-page.read`, follows the set's exact ordered page digests and
shows an integrity error rather than partial content on a missing/crossed page.
Target reprepare rejects an unresolved old action. Optional chair annotation is
non-gating and separate: its exact disposition is `substantiated`,
`unsubstantiated`, `duplicate` or `needs-more-evidence`. The Console shows the
one current append-only annotation revision/note/digest beside live evidence;
it cannot change or appear in completion or receipt v2. The mutation receipt is
immutable; currency appears only on fresh read/list/operator projection.

The Console reads review state only through its scoped operator Evidence
row/detail projection. Rows show slot/head, target chair/profile,
current/superseded, certifying, answer safety or terminal-failure code,
CLEAN/FINDINGS/UNUSABLE, P0-P2/open counts, provider/model, reviewer-family relation and
artifact/bundle/coverage/answer/result digests. Detail adds task/action, safe
pair, current annotation, active/action binding certification cut, paged
findings, route/final-prompt and publication lineage. A third recovery-action
row carries only the live recovery projection and retirement eligibility.
Malformed fields reject
the whole variant.

The Evidence header displays the exact `reviewCompletionV1` response as
`Final review: Complete` or `Blocked`, including target chair, artifact,
bundle/coverage/profile digests, top-level ordered blockers and, when a target
profile exists, its four slot-head rows. Missing target or unavailable profile
uses the closed empty-slot branch, so the header still shows the reason.
Unavailable required capability uses the distinct typed blocker/slot list above.
Finding-capacity exhaustion uses its target-wide empty-slot branch. A
trustworthy target whose binding/profile/head structure is corrupt retains its
immutable target/artifact/bundle fields but shows chair/profile null, empty
slots and top-level integrity-failure; multiple/no trustworthy targets use the
target-null integrity arm. Top-level and slot blocker vocabularies are disjoint,
and historical superseded currency is never shown as a completion blocker.
`open-findings` is the only finding blocker;
`provider-terminal-failure`, `terminal-no-effect` and `retired-unknown` are
distinct from `ambiguous-action`. The Console
does not recompute, choose a latest timestamp or hide a blocker.

Every certifying action is recovery-owned before generic provider recovery.
Prepared zero-dispatch proof closes no-effect; dispatched/accepted ambiguity
gets at most one pair lookup; valid answers use the same automatic terminal
transaction. Unresolved effect reaches the direct-human retirement gate above.
The Console never offers a generic retry/resume for that custody. It displays
the live scoped recovery-action row with pair, target/slot/attempt, recovery
generation/state/reason, reservation, route/lookup/settlement/evidence fields.
Retire is visible only when that live row says `awaiting-human-retire` and
eligible; Preview/Commit binds the same generation/state/reservation. Receipt
recovery history is audit-only and never enables an action.
The schema-v2 Fabric receipt is specified to export this same reducer under
`reviewCompletion`, exact safe route/review rows under `providerRoutes` and
`providerReviews`, and safe recovery digests under `routeIntegrityRecoveries`.
That extension is not yet in the shipped closed schema
(`runtime/agent-fabric/schemas/fabric-receipt.v2.schema.json`); until it lands
the Console must not invent alternate receipt
fields. Receipt v2 validates standalone with literal local objective/provider/
operator catalogues and no resolver; unknown future codes fail instead of
being displayed as current truth. Raw provider-specific detail stays private
behind its evidence digest.

A direct Claude, Cursor, Gemini or other provider CLI may be used only when
Fabric is unavailable and the chair records the degraded reason. Its output may
be registered as an ordinary review artifact for diagnosis, but it is visibly
`non-certifying`, cannot create provider-review evidence and cannot satisfy the
fresh other-primary, cross-family or no-unresolved-P0-P2 completion gate. The
review must be rerun through Fabric before completion; post-hoc import,
self-attested family and caller-selected independence are unavailable.

## User-attention policy

User intervention is required for judgement or consequential boundaries, not
ordinary replanning. At minimum, gates cover:

- unresolved intent or material subjective trade-offs;
- spec and one-way architectural decisions;
- new trusted roots or confidential-data disclosure;
- provider login or credential entry;
- destructive Git/history operations;
- non-pre-authorised push or merge;
- release or deployment of an artifact or target not covered by its exact
  final-acceptance and release gates;
- external communications;
- irreversible actions and final acceptance.

Routine topology, model, worktree, test and implementation changes shall not
create blocking gates. Material authorised changes may notify without blocking.
A pending task-scoped gate blocks only its dependent subtree or barrier;
unrelated runnable work continues.

Each gate therefore records `scope_kind` (`task`, `subtree`, `run` or
`release`), affected task IDs, dependency revision, blocked operation IDs and
enforcement points (`task-readiness`, `operation` and/or `scoped-barrier`). The
daemon checks gates before task claim/start/resume, before a named consequential
operation and before scoped-barrier closure. The affected task and, when the
scope requires it, its dependent descendants are blocked; unrelated siblings
remain runnable. Run closure evaluates the union of scoped barriers and does
not treat an unrelated task gate as a global stop. Gate creation/resolution and
dependency changes are transactional, and negative tests exercise each
enforcement point.

Every gate shall include the question, affected run/task/subtree, reason,
options, recommendation, consequences, evidence, revision, approver and any
deadline/default. No gate auto-approves on timeout.

The Console shall permit typed responses and natural-language responses. Every
operator command shall carry the operator capability, a stable command ID,
expected revision, actor, provenance and before/after audit event. The daemon
shall authorise the exact action and project before mutation. Duplicate
commands have one effect; stale revisions fail closed and show the changed
state. Negative tests shall cover absent, expired, revoked, wrong-project,
wrong-generation and action-insufficient operator capabilities.
