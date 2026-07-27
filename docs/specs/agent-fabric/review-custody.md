# Agent Fabric review bundle and portal custody

## Artifact-content read boundary

the bounded artifact-read contract owns the public operation and result semantics. This section owns its daemon implementation,
filesystem containment, bounded codec, negotiation and restart behaviour. It adds no artifact authority and no second
artifact store.

The operation registry and generated protocol manifests shall advertise `artifact-registry.v1` /
`fabric.v1.evidence.publish` and `artifact-content-read.v1` / `fabric.v1.operator-artifact-content.read` only when their
complete closed codecs and daemon handlers are available. A client without the latter exact feature/operation has no
`artifacts.readContent` surface. Feature absence is an honest unavailable state, not a fallback to direct filesystem
access.

The handler uses two short SQLite transactions with bounded filesystem work between them. It never holds a database
transaction or the synchronous daemon owner across file I/O:

1. phase A authenticates the `afop_` credential at point of use for the exact project, optional session, current
   principal generation and `read` action;
2. phase A selects one active `artifacts` registration, compares its revision and complete ref, captures the exact
   project/session/run/source/publisher tuple and derives its trusted source root;
3. outside SQLite, the daemon canonicalises that root, rejects traversal and opens the exact regular file read-only with
   a no-follow primitive;
4. it rejects a symbolic link, link count other than one, non-regular file or any pre-open/post-open path, device or
   inode mismatch;
5. it reads at most 1 MiB plus an overflow sentinel, rechecks device, inode, size and modification time and verifies raw
   source SHA-256 before strict UTF-8/media validation;
6. it applies whole-artifact terminal/credential safety transformation, bounds the inert rendering, validates the cursor
   and returns one monotonic UTF-8- bounded page with complete-rendering/page digests and an exact whole/start/
   middle/end line-fragment label; and
7. phase B opens a fresh transaction, reauthenticates every credential/ principal/project/session generation and
   compares the captured evidence, source-owner/root and ref tuple immediately before response. Any change is `stale`.
   Unrelated global Fabric activity is not an artifact-content fence.

A second database connection must be able to commit while a deliberately slow filesystem read is between phase A and
phase B. The final transaction must see that connection's relevant changes; SQLite snapshot reuse or event-loop
serialization is not proof of stability.

Source routing is closed and registration-owned. `project-file` joins the canonical project root and is admitted only
when an authenticated agent's artifact-path authority covered the path at registration. `run-file` joins the project
root to the run's normalised project-relative artifact directory; content projection requires that directory to be a
dedicated strict descendant of the project root. `git-private-diff` joins the configured canonical daemon- private root
and exact reserved `private/git-diffs/<source-digest-without-prefix>.patch`; only the fixed Git service may register it.
Caller values never select a route or root.

The daemon shall not resolve through process current directory or a symlinked ancestor. It rejects absent/non-canonical
roots, sensitive path classes such as credential stores, VCS internals and environment/secret files, and any
`project-file` registration outside its sealed publication authority. A platform that cannot prove the no-follow and
identity invariants reports the operation unavailable. Reading never shells out, executes a renderer, follows an
include, invokes a pager or parses project-controlled configuration. JSON validation is an in-process bounded syntax
parse only. Markdown, diff and plain text are projected as inert text; they are not rendered into terminal control
sequences.

The source inspection ceiling is independent of the caller's response limits. `maximumBytes` (`4..131072`) and
`maximumLines` (`1..2000`) may narrow the response but never widen the 131,072-byte, 2,000-line page maxima, 1 MiB
source ceiling or 2 MiB inert- rendering ceiling. Safety transformation precedes pagination. Each cursor is a bounded
integrity-protected, stateless encoding of the exact evidence revision, source/rendered digests, algorithm version, page
index and next rendered byte/ boundary. The pager prefers the last LF within the requested byte limit; when one logical
line exceeds that limit it advances at a UTF-8 code-point boundary and labels the fragment without changing the complete
rendered line count. It expires when any binding changes and cannot be used to skip, repeat or reorder a page as a
complete review. The handler retains no source bytes after the response and writes no cache, event, acknowledgement or
audit row merely for reading. Ordinary bounded request telemetry may record only the operation name and closed error
code, never content, path-derived filesystem authority or credential text.

The shared message/artifact redactor derives current bearer families from the credential registries and includes `afb_`,
`afc_` and `afop_` as mandatory canaries. Its versioned daemon-owned credential classifier also covers exact
runtime-known secret values, private-key blocks, authorisation headers, URL userinfo, recognised cloud/provider token
forms and assignment values whose closed key vocabulary denotes password, token, secret, credential or private key. It
replaces a complete classified value before pagination and cannot leave a prefix, suffix or length-correlated fragment.
If a sensitive construct cannot be boundedly classified/redacted, the result is `unsafe-content`, not a partial
rendering. This deterministic vocabulary is a safety boundary; project content cannot add or remove patterns.

Terminal neutralisation covers CSI, OSC, DCS, APC, PM, SOS, C0/C1 controls, carriage-return rewrites, bidi overrides and
other sequences able to alter or disguise the operator display. Newline and ordinary tab semantics may be preserved only
within page bounds. The source, complete rendered and page digests are calculated over their explicitly named byte
domains after the closed transformation order.

Migration 0010 rebuilds `artifacts` as the one evidence metadata registry while leaving all bytes with their existing
owners. Additive closed columns are exact `project_id`, nullable `project_session_id`/`run_id`/`task_id`, publisher kind
and ref, source kind, evidence kind, canonical prefixed SHA-256, registry state, quarantine reason and positive
revision. Active source/scope/path/digest are immutable. Partial unique indexes enforce one project-, session- or
run-scoped identity. `project-file`, `run-file` and `git-private-diff` have disjoint CHECK shapes and producer-owned
namespaces. Evidence projection reads only active rows and takes kind, revision, ref and provenance from this registry
rather than hard-coding them.

The squashed baseline `artifacts` table declares exact `UNIQUE(artifact_id, revision)` in addition to its `artifact_id`
primary key. That apparently redundant composite key is mandatory: every immutable evidence child in the review-custody contract uses
the exact two-column registration revision as a SQLite foreign-key parent. A child can never cite a revision value
merely because the artifact ID exists.

The current baseline stores only the wire `sha256:` artifact digest form. Publication applies the closed source
classification; result completion must prove the replying agent's persisted path authority and rejects an unprovable
root-equal registration. `fabric.v1.evidence.publish` and every other registry producer apply that same
derive/reclassify-or-reject rule regardless of the requested source kind. A database invariant/postflight query rejects
every active `run-file` whose normalised run root is `.`. Invalid paths or digest-identity collisions likewise remain
explicitly quarantined and unprojected rather than crashing a codec or guessing provenance. Existing receipts and intake
bindings gain exact registry IDs. Every new intake binding has a foreign key and trigger proving its repeated
path/digest equal one active same-scope registry row.

`intakes` and `intake_revisions` gain an accepted-scope registry ID and closed state. New accepted revisions require the
one explicit registered `acceptedScopeRef`; other states forbid one. Zero, multiple or quarantined candidates are
rejected; the runtime never chooses the first ref. Changing accepted scope increments the project revision in the same
transaction so Project row/detail references cannot remain current.

The baseline has an explicit run-directory basis. Operator-launched relative roots resolve only beneath joined
`projects.canonical_root`; absolute run roots are not admitted. Outside, ambiguous or symlinked roots fail before state.
One shared `resolveRunArtifactRoot` replaces direct/cwd-relative use in publish, results, receipts, checkpoints,
provider evidence, retention and content reads.

Preflight stages every normalised row and binding before table replacement; postflight runs foreign-key/integrity
checks, identity/count reconciliation, canonical path/digest queries and registry-trigger probes. Fault injection at
each staging, rebuild, binding and migration-record boundary exposes the complete old or complete new schema.
`artifact-registry.v1` and `artifact-content-read.v1` advertise only after postflight passes. the Console contract owns all Console
paging, disclosure, acceptance and viewport behaviour; this spec owns only the daemon/client capability boundary.

Deterministic verification additionally covers:

- zero filesystem I/O for wrong/expired/revoked credential, action, project, session, generation, evidence revision, ID,
  ref or cursor;
- exact project/run/private source routing and rejection of caller-selected source/root, arbitrary, absolute, traversal,
  sensitive, replaced-ancestor, symlink, hard-link, FIFO, device and socket paths, including races before open, during
  read and before response;
- source digest mismatch, size growth/shrink, inode replacement, invalid UTF-8, NUL/binary content, malformed or deeply
  nested bounded JSON, unsupported extension, unsafe credential construct and source/rendering overflow;
- byte/line caps at below, exact and above bounds, empty source, CRLF, combining/multibyte characters and
  transformation/page boundaries, including complete multi-page reconstruction and duplicate/skip/reorder/cross-ref
  cursor negatives;
- every terminal family, bidi control and bootstrap/agent/operator capability plus
  private-key/auth-header/URL/provider/assignment canaries proving literal safety flags only when output is inert and no
  credential fragment remains;
- untransformed source/rendered/page digest equality and independent transformed complete-rendering and per-page digest
  verification;
- concurrent credential/session/evidence/source-root/file changes producing only `stale`, never mixed/current content,
  while unrelated global activity does not starve a valid read;
- a second connection committing a relevant change between the two short transactions, and a writer completing during
  slow filesystem I/O;
- operator-relative roots, prefixed digests, receipts/intake bindings and accepted scope; invalid/ambiguous roots fail,
  while unrepresentable artifacts quarantine without parser crash;
- idempotent authorised project/run publication, result/receipt registration, private Git-diff registration and exact
  intake/gate/acceptance binding, with cross-scope/unregistered refs rejected atomically; root-equal requests from every
  producer reclassify only with exact authority proof or reject, and no active root-equal `run-file` survives direct
  SQL/postflight checks;
- negotiated client presence/absence, malformed closed variants, restart and at least 32 concurrent bounded reads
  without unbounded memory, descriptor drift or database writer starvation; and
- the production Console evidence workflow over every source kind, with raw terminal output free of controls and
  credential canaries.

## Complete-review custody, linear heads and route recovery

the certifying-review contract owns public behaviour. This section owns the current baseline relations, private content store,
transaction boundaries and recovery. There is no compatibility import.

### Publication and eligible delivery source

artifact_publication_lineage is insert-only and one-to-one with an artifact revision. Its canonical JSON mirrors
artifactPublicationLineageV1 and normalised columns include publisher agent/principal/bridge generations, provider
custody adapter/action, provider-session generation, adapter contract, family, model, route receipt digest, state/reason
and lineage digest.

Both chair launch activation and retained-child activation write the same immutable provider_session_lineage row. Its
owner discriminator joins either launched_chair_bridge_state plus project-session launch custody, or child
agent_bridge_state plus provider-agent custody. An agent publication joins its authenticated principal/bridge generation
to exactly one such active row. Composite foreign keys require one run, agent, principal generation, bridge, provider
session and adapter contract. Family/model are mandatory; route digest is nullable only when that launch/provider
custody owns none. Zero, multiple, absent or crossed joins insert unproved with the exact closed reason. No caller field
can make it proved. Update/delete triggers make all lineage content immutable.

Only an active project-file or strict-descendant run-file registration of explicit evidence kind
implementation-delivery-manifest.v1, published by an agent with proved lineage, is target-eligible. certifying-review-four-slot-v1
additionally requires an equality join to implementation_delivery_manifests and the one current delivery_review_bases
row produced by fabric-seal, and that publisher family equal target-chair family. A generic artifact registration
carrying the same kind/content cannot satisfy that join. git-private-diff, operator-, Fabric- and project-published rows
remain valid bundle evidence but are never silently promoted to eligible root targets.

The current evidence/artifact-kind catalogue explicitly contains delivery-requirement-map.v1,
implementation-delivery-manifest.v1, coordination-gate-snapshot.v1 and discovery-surface.v1; none is inferred by parsing
generic receipt content. The first three use the persistence owners below. Discovery-surface.v1 is registered only by
the review-custody daemon renderer and is rejected by public/agent evidence publication.


Run start is immutable; AFAB-004 stores the full c2fc623a2529f87feca27982e1a140969ab5a258 base. Snapshot/manifest
content has no self-digest or final-basis reference. `fabric.v1.implementation-delivery.seal` implements replay and
stable run/delivery-run single-flight before work. Phase A captures the exact chair principal/lease/bridge/session
lineage, run-start, delivery RUN/scope/full requirement-map entries, profile, authority/gate/ evidence/artifact
revisions and Git HEAD/index/worktree tokens. The request's expected HEAD is only an optimistic lock; no caller field
selects base or closure content.

Before manifest seal, `fabric.v1.delivery-requirement-map.seal` derives the one current closed map from accepted scope
and the checked-in console-acceptance-delivery-requirements.v1 catalogue. Only the authenticated current chair may call it. Its
closed request contains command/project-session/run/delivery- run IDs, expected current map generation (zero iff none),
expected current accepted-scope revision and expected checked-in catalogue digest. A wrong zero/ positive/current
sentinel conflicts before work. It equality-CASes catalogue/ scope/source/evidence revisions, requires every binding ID
exactly once and proved, and registers delivery-requirement-map.v1 bytes. Stable run/delivery- run single-flight
precedes phase A: exact command replay returns its immutable result, changed replay conflicts and different commands
serialize. Before generation allocation, `closure_digest` hashes RFC 8785 JCS of the complete prospective map with only
map generation and closure digest omitted. Equality with current returns its existing bytes/registration/generation;
otherwise the daemon allocates current generation plus one and inserts. Only changed catalogue,
accepted-scope/binding-source or selected evidence closure advances generation; command-ID churn cannot stale a basis.
delivery-run v1 remains unchanged; no mutable RUN.json digest becomes a review-basis dependency.

Outside SQLite, fixed no-follow readers validate the complete profile-derived requirement/evidence closure and clean
base-to-HEAD state. They write the coordination snapshot and manifest to reserved digest-named run-file CAS paths
create-exclusive, fsync, re-read and verify. Phase B reauthenticates, equality- CASes every captured row/token and
atomically inserts both artifact registrations, authenticated-agent publication lineage, final review-basis row and
immutable command receipt. `producer_kind=fabric-seal` is distinct from the authenticated agent publisher. Failure
leaves no DB row; unreferenced CAS bytes are run-owned GC. Exact replay returns the stored result; changed replay
conflicts. Any source/scope/map/profile/gate/evidence/lineage revision makes the basis stale and requires a new seal.

### Bundle store and target transaction

The private bundle owner uses these logical current relations:


Normalised changed-file and required-evidence rows plus finding-set/page/member relations own the complete sorted
coverage manifest and foreign-key every object they name. Finding members include the immutable safe record, exact
origin action/result/manifest/basis/bundle and source/evidence/mixed repair currency. Successor checks require later
manifest/basis/bundle for every finding, source advance only for source/mixed and each named evidence revision plus
changed content for evidence/mixed. Identical-byte or Git-only evidence repair fails. Checks enforce the Agent Fabric contract counts
and byte limits, exact before/after/diff shapes per Git status, contiguous object/chunk/manifest-page ordinals and one
computed coverage, manifest-body, bounded-root and bundle digest. Identical chunk bytes may recur at multiple ordinals;
the ordinal key preserves the object sequence while the private CAS deduplicates physical storage by chunk digest. Each
page is at most 65,536 bytes, there are at most 16 pages/1 MiB, and the root is at most 49,152 bytes and lists every
ordered page digest. Empty objects have no chunk; every nonempty object has complete contiguous chunk coverage.
Insert/update triggers reject partial manifests and make every committed row immutable.

Finding pages are exact RFC 8785 JCS, contain whole strictly ordered unique members and are at most 65,536 bytes.
Set-page ranges are contiguous, nonoverlapping and equality-copy page count/member/range data; the root count is the
exact sum. No fixed finding-count cap exists. Each bundle foreign-keys one complete set root and all its pages as
mandatory objects. A normal action capacity reservation proves room for 32 maximum-size new records and resulting set
roots before router I/O. `resolution-only` stores zero maxima and can only remove up to 32 prior digests; it is
noncertifying. Triggers reject result insertion beyond reservation. Physical minimum-root/page exhaustion creates the
typed operator gate; no referenced finding row is deleted or overwritten.

Changed-file rows additionally store exact status, old/new UTF-8 paths, before/after mode, object and byte-length arms
plus `diff_object_digest` under the review-diff.v1 codec in the Agent Fabric contract. Target preparation must compare
the checked-in codec and rules digests and immutable conformance fixture manifest before a worker claim. That
startup/prepare gate is not yet wired: the current review-diff builder accepts caller-supplied syntactically valid
digests and verifies only the set's internal consistency. The fixture manifest binds full base/head object IDs, object
format, source-object-set digest, exact expected counts/bytes and diff-set digest. The fixed Git reader disables mutable
config and implements exact-content rename pairing plus the closed Myers/binary arms; it never parses porcelain output.
Triggers enforce arm nullability, path/status ordering and equality between the bundle's stored codec/rules/set digests
and the complete child set.

Generated canonicalisers own the exact the Agent Fabric contract preimages for requirement-map, evidence-closure, repository-source-state,
coverage and mandatory-read-set digests. Stored map/evidence/source/object digests equality-copy their registered bytes.
Child tables enforce every body ordinal plus changed-file, evidence, object, finding-page and mandatory-entry
order/uniqueness. Golden vectors and permutation negatives cover every domain. Startup/prepare must compare the
generated-code, schema and vector digests and disable seal/prepare before filesystem work; that gate is not yet wired.

Digest construction follows the Agent Fabric contract's acyclic order: JCS manifest body with no self/later digest -> raw body pages ->
JCS root -> JCS final bundle ref; each digest is stored outside its own bytes. The mandatory set is root + every
manifest-body page + finding-set root/page, and the delivery manifest/map plus required
accepted-scope/spec/ADR/decision/gate- decision/coordination-snapshot objects. Target commit rejects more than 80 unique
root/page/chunk responses or 6 MiB mandatory bytes. Limits are 4,096 changed paths, 1,024 evidence rows, 16,384 objects,
32,768 deterministic 64-KiB chunks, 16 MiB per object, 64 MiB unique object bytes, 4 MiB search index and 256 KiB
risk-map output. All changed-file diffs and other evidence remain completely available. The final target recomputes
exact review-diff.v1/body/object/wire bytes from its immutable approved run-start to actual sealed HEAD; with the full 2
MiB risk- sample ceiling it must materialise under 6 MiB mandatory/10 MiB combined wire bytes. No earlier delivery-HEAD
count is a gate. The immutable pre-codec sizing observation for
`c2fc623a2529f87feca27982e1a140969ab5a258..0a04d161c5d4fa027c96410b3cc0cf887e1c6e42` is 601 changes, 1,434 objects,
27,766,213 bytes and largest object 4,097,314 bytes; it is deliberately not stored as final target expected output. The
daemon also writes an immutable bundle-search.v1 index and applies the checked-in review-risk-map.v1 to every manifest
entry. The rules score and sort changed objects deterministically, then select exact highest-risk diff chunks from every
nonempty group, at most 32 chunks/2 MiB total. Those caller/provider- independent sample digests join the mandatory set;
target prepare fails if the whole mandatory set cannot remain within 80 reads/6 MiB. Literal search is available for
deeper exploration but never substitutes for the sample; it is limited to 16 calls/1 MiB aggregate response per action.
Target state binds search/risk/sample/mandatory digests and budgets. Each target owns one logical bundle/root;
pages/chunks are internal and CAS reuse never creates a bundle chain. Bundle digest covers
manifest/object/search/risk/mandatory components. This coverage is transitive through body -> root -> final ref; no
digest domain contains itself.

The required coordination-gate-snapshot.v1 object is produced by the seal owner above. It excludes
review/final-acceptance/release/final-receipt state. fabric-receipt.json is never a bundle input and cannot
advance/stale the basis.

Target preparation is a durable daemon job. The public acceptance transaction authenticates the current chair; checks
the exact zero/current target sentinel, task, eligible manifest row and persisted four-slot capability availability;
runs command replay and active semantic-digest join/conflict; increments the run's preparation/target/bundle high-water
row; inserts one immutable `prepared` row with every database precondition and accepted-receipt digest; and returns. It
performs no Git, evidence, CAS-store, provider or network I/O. The operation therefore cannot spend the public 30-second
deadline building a 64-MiB closure. A missing slot capability fails `CERTIFYING_REVIEW_CAPABILITY_UNAVAILABLE` before
preparation insert and remains visible in completion availability rows.

`semantic_input_digest` covers run, authenticated actor/principal and the full closed request with command ID omitted
only for active-job joining; `full_input_digest` includes the complete request and owns command replay. One partial
unique index admits one active `prepared|building|built` row per run. The same semantic digest under another command
joins the existing accepted receipt; a different digest conflicts before high-water update. Reserved target and bundle
generations are never reused after any terminal outcome.

A bounded FIFO worker claims `prepared` by incrementing `worker_claim_generation` and assigning a leased daemon
instance, then moves it to `building`. It captures the eligible delivery artifact/lineage, sealed review basis, adopted
current chair/provider binding, activated adapter contracts/profile schema, exact trusted Git base/head/index/worktree
state and all four predecessor head/attempt/open/repair tuples. Outside SQLite it uses the fixed Git/evidence readers to
enumerate every review-diff.v1 change, required evidence and complete carried finding; reads exact bytes no-follow;
builds all objects/index/pages/root; writes create-exclusive CAS content; fsyncs and re- reads. A verified complete
build moves `building -> built` with immutable digests. Build failure moves to failed. No filesystem work occurs while a
write transaction is open.

Phase B for `built` reauthenticates and equality-CASes every captured tuple and all four heads. Preparation and
lifecycle rotation serialize here. If same- agent lifecycle adoption occurred during build, Phase B may create
generation- one against that adopted current binding only when adapter/contract/family/
model/profile/task/artifact/basis/source/bundle are unchanged; otherwise it commits `chair-binding-changed`. Existing
effect ambiguity keeps the row built and fenced at Committing while route recovery proceeds; lifecycle adoption is never
blocked. A changed/new nonterminal predecessor tuple conflicts as `predecessor-action-nonterminal`. Success atomically
inserts the reserved bundle metadata/coverage, supersedes the old target, inserts one current immutable
`review_completion_target`, generation- one chair binding/head, resolved profile/slots and four generation-zero review
slot heads, then transitions `built -> succeeded` and stores the target ref. The only other Phase-B outcomes are
conflicted or failed; no partial target is visible.

The only preparation edges are `prepared -> building`, `building -> built| failed` and `built ->
succeeded|conflicted|failed`. State, terminal code/evidence and target-ref triggers enforce the exact the Agent Fabric contract union.
Conflicts are only target-generation, chair-binding, task-or-authority, delivery-basis, repository- source, profile,
predecessor-head or predecessor-action change. Failures are only bundle-too-large, unsupported-repository-state,
source-read-failed, content-integrity-failed or certifying-capability-unavailable. Succeeded carries only target ref;
nonterminal carries null. Public `review-target-preparation.read` is an indexed read of this row and accepted receipt.
It maps the three nonterminal states to Preparing, Building and Committing and exposes no invented percentage. Progress
is required as either phase-only or finite verified-build-items. A finite plan writes immutable plan-digest/total once;
completed may only increase after the corresponding item fsync and re-read and must equal total before built. Triggers
reject downgrade, total/plan change, regression or completed above total.

ReviewTargetPreparationRecoveryService runs before private-CAS garbage collection and generic jobs. It reclaims an
expired worker lease by advancing the same claim generation. Prepared restarts at build; building validates and reuses
only exact digest-verified CAS bytes; built reruns only Phase B. It never allocates another generation or creates a
second target. CAS GC excludes every digest reachable from an active preparation, target or bundle; unreferenced bytes
become eligible only after the owning preparation is terminal. PID/daemon restart and fault injection at every
write/fsync/state/Phase-B statement prove one resumable row or one complete target.

`review_completion_targets` stores exact preparation/target generation, review-subject digest, task, delivery
artifact/lineage, review basis/source state, bundle/coverage/manifest, resolved profile/schema, bundle-search/risk-map
and mandatory-read-set/count/ byte digests plus initial chair-binding digest. It does not duplicate mutable chair
generations. A partial unique index permits one current row per run. Drift never updates it; reads derive stale. The
only current-to-superseded update occurs in successful successor preparation Phase B. Every operation invokes the same
pure currency predicate and active binding join. Reads derive stale-target without a write/global-revision advance. A
new dispatch or optional annotation rejects stale currency. The action-bound terminal transaction still settles and
advances its reserved head. Only a newly succeeded preparation supersedes the old target.

`review_target_chair_bindings` is insert-only. Generation one is created with the target; later generations require the
exact prior binding foreign key and one finalized adopted `lifecycle_rotation_custody_revisions` row for the same agent
whose head points to that exact journal revision. Triggers require contiguous generation and equality of adapter,
contract, family, model, profile, task, artifact, review basis, repository source and bundle. They require the exact
predecessor binding digest and certification-cut custody/row/digest/sequence; the cut custody must equal the binding's
adopting `(run_id,agent_id,lifecycle_custody_id,lifecycle_custody_revision)` ref. Generation one has all
predecessor/cut/custody fields null; every successor has all of them nonnull. They permit only principal, chair-lease,
provider-session, bridge and route-receipt generations to advance. `review_target_chair_binding_heads` is the sole
active pointer and advances by one CAS. A different agent or any non-generation binding change cannot insert and leaves
the target stale.

`review_target_rebind_receipts` is insert-only and unique by run/target/exact agent/custody/revision ref plus command
replay. It stores the exact the Agent Fabric contract receipt and digest, prior/new binding generations/digests, immutable
subject/bundle/profile digests and before/after head/open/repair set digests. Both tables equality-copy the exact target
`review_subject_digest`; triggers reject any receipt or binding whose immutable target fields do not reproduce that
digest. The public `review-target.rebind` transaction authenticates the current chair, derives the
target/custody/current-binding tuples, rechecks every immutable subject field and four head/open/repair tuples, then
inserts the successor binding, advances the pointer and records the receipt atomically. It performs no router/provider/
portal/lookup I/O. The true-chair adoption transaction invokes this same store mutation directly; an exact later command
joins the existing custody-keyed receipt. Wrong or non-adopted custody, crossed agent/generation/subject, stale
pointer/head, duplicate generation or changed replay changes nothing.

Every successor binding, certification cut and rebind receipt equality-copies the Agent Fabric lifecycle review-decision
digest and externally authenticated receipt digest. They are inserted only inside the post-authority lifecycle apply and
composite-reference `lifecycle_review_authority_bindings`, which in turn binds the exact immutable reservation,
ordinal-two intent/authority receipt, finalized custody revision and apply. Subject custody, adoption evidence,
decision, cut/null and linked recovery-loss decision byte-equal the reserved rows. The separately verified
scope/namespace checkpoint proves external membership; a point read alone cannot. Missing authority/row, stale chain,
crossed receipt or verification failure is lifecycle integrity failure and inserts no review row. The mutable lifecycle
decision audit is corroboration only.

Every certifying first terminal transaction increments `review_terminal_sequence_high_water` and stores that stable
sequence in the terminal journal/result digest. True-chair lifecycle adoption reads that high- water in its own
serialization transaction, inserts the exact custody-keyed certification cut and either appends/activates a same-subject
successor binding or leaves the target read-derived stale. Review state never rejects or rolls back adoption. A later
stale adoption may append another cut for the same target/predecessor because the exact agent/custody/revision ref, not
predecessor generation, is the primary identity; the unique cut digest and exact successor foreign key prevent reuse
across custody. Old-binding prepared/zero-dispatch attempts fail their worker currency check and the route-recovery
owner terminalises them no-effect once; dispatched/accepted/ ambiguous attempts recover normally. Evidence certifies
through a successor only when its terminal sequence is at or before the first successor cut and the complete binding
chain/digests are contiguous. Later terminals remain adverse and permanently noncertifying. No
target/head/evidence/finding row is cloned or rewritten. Broken chains/cuts or multiple active pointers are
integrity-failure.

Evidence stores only `certificationBasisAtTerminal` and its immutable receipt digest. Read/list, operator projection and
completion derive a separate `currentCertificationBasis` from the active binding chain. Rotation may change that live
arm from active-binding to predecessor-cut without rewriting evidence; a terminal after the first successor cut uses the
exact post-cut arm and is permanently noncertifying. A broken/missing chain yields null live basis plus the existing
integrity/stale blocker, never a fabricated predecessor-cut.

`review_finding_capacity_reservations` is a pre-router child of the pair preflight, not of `provider_actions`. Its
closed state is `preflight|attached|released|settled`. The pre-router row binds the global pair, run, target, closed
slot and owner/reservation digests with null attempt generation. After a successful resolver result, the one binding
admission/ dispatch transaction from the Agent Fabric contract CAS-increments the slot head, assigns that positive attempt to the
reservation exactly once, inserts action and route, and moves the reservation from `preflight` to `attached`; none can
commit alone. That null-to-positive attach is the only tuple finalisation. Thereafter run, target, slot, attempt and
digest are immutable. Resolver/admission failure moves the reservation to `released` with attempt still null, returns
its physical capacity exactly once and creates no provider action, route or budget row. Exact retry observes the
released route failure; a new action pair may reserve normally. Startup releases only expired preflight rows after
proving no matching action/route, while attached rows remain owned by terminal/recovery settlement. Released/settled
rows are immutable audit/replay history and consume zero live capacity. Thus only successful dispatch consumes a
contiguous attempt generation, exactly as the certifying-review attempt and evidence clauses require. After attach, every terminal
branch writes only `settled`, including the `proved-no-effect-release` disposition: that disposition returns the
complete physical capacity but does not use the pre-admission `released` state. `released` is reachable only from
`preflight` with null attempt generation; no attached path nulls or changes its attempt custody.

The append-only availability revision/head tables are the safe current activation projection keyed by the complete
project-session/profile/schema/ target-family/slot/adapter/contract/family/model/source/runtime/platform tuple. Each
revision is available with null reason or unavailable with exactly one of `adapter-inactive`, `contract-mismatch`,
`confinement-unproved`, `portal-unavailable` or `provider-runtime-unavailable`. Adapter activation, canary or contract
change appends a revision, CAS-advances its head and global revision in one transaction. Target-preparation admission
and completion use this same table; neither infers capability from a missing target or raw adapter error.

review_profile_snapshots and review_profile_slots normalise the exact four-slot target snapshot. The checked-in
schema/profile catalogue digest covers, in fixed order, `runtime/agent-fabric/schemas/review-profile.v1.schema.json`
then `config/review-profiles/certifying-review-four-slot-v1.json`, represented as repository-canonical JSON with their
stable relative paths. Before spawning a new daemon, startup verifies those on-disk documents against the checked-in
digest and fails closed on mismatch, invalid JSON or absence. The caller-supplied `profileSchemaDigest` remains the
wire claim and availability foreign key, but profile resolution accepts it only when it equals that authoritative
checked-in digest; resolution also verifies the caller-supplied profile object against its generated member digest,
so it cannot resolve different content under that catalogue digest. `npm run profile:catalogue:pin` is the deliberate
regeneration command. Slot rows byte-match
resolvedReviewProfileSlotV1: adapter
class/ID/ contract, family/model, requested/tagged resolved effort, aliases, source/runtime/platform identity,
provider/internal-step/read ceilings and explicit reviewer-family relation. Publisher eligibility remains the separate
proved lineage/family-equals-target predicate. The baseline requires exactly native, other-primary, cursor-grok and
agy-gemini and enforces the exact the Agent Fabric contract mapping. Native is exempt; all three external slots require reviewer family
distinct from target-chair family. No publisher-independence column/blocker exists. Missing or extra slots prevent
target commit.

The action-pair-only portal authenticates an ephemeral capability hash bound to adapter/action pair, target, bundle,
coverage digest and expiry. Its MCP server name is agent-fabric-review-bundle and its only tools are review_bundle_read
and review_bundle_search. initialize/initialized, ping and tools/list/call are allowed. resources/list,
resources/templates/list and prompts/list return exact empty arrays; resource read/subscription, prompts/get and
sampling/roots/ completion/elicitation/logging are denied. It reads only committed root/page/ object/chunk joins and
verifies their complete digest chain.

Read payloads use RFC 4648 padded base64. Raw root/page/chunk bytes are at most 65,536 and encoded payload is at most
87,384 bytes. There is no independent metadata allowance: generated closed response templates bind every field, maximum
value, escaping rule, JSON-RPC envelope and final LF and prove the complete read response is at most 98,304 bytes.
Requests are exactly one UTF-8 JSON object plus LF, with no BOM, CRLF, batch, duplicate key or trailing bytes. ID is
integer `0..2147483647` or an ASCII string matching `^[A-Za-z0-9._:-]{1,64}$`; response is exact RFC 8785 JCS plus LF.

Preparation reserves every mandatory response using the maximum 64-byte string ID sentinel, then materialises it after
bundle digest construction. Runtime journal/ledgers debit the exact complete response for the admitted actual ID. Direct
dynamic-tool transports use the identical equivalent JSON-RPC charge and Fabric-assigned action-local integer ID when no
provider ID is exposed. Search retains its separate 65,536-byte response limit. Generated exact-bound fixtures cover
both ID arms, binary/page/root/empty/error/search responses and reject any runtime byte count above reservation before
activation.

The read journal owns separate nonfungible mandatory and exploration counters. The first response for each mandatory
digest debits mandatory; duplicate/ optional reads, search and authenticated malformed/out-of-bundle calls debit
exploration. Direct mode reserves mandatory <=80 calls/6 MiB plus 32 calls/4 MiB exploration; helper mode reserves
mandatory plus 48 calls/4 MiB. Both have a 10 MiB combined wire-byte ceiling. Search is inside exploration and is
bounded to 16 calls/1 MiB aggregate plus 256 query bytes, 100 results, 65,536 result bytes and 250 ms CPU per call. Each
row stores subledger/ordinal/tool/request/result/status/exact canonical response bytes. Unique root/page/chunk responses
alone satisfy mandatory; call order is free. Portal-helper stream/hook events join the same journal. The portal resolves
no caller path or filesystem locator. Response/prompt bounds apply before adapter I/O.

## Portal launch and filesystem custody

### Adapter capability and route admission

adapter-compatibility activation gains one digest-bound closed capability, certifying-review-packet-only.v1. Its
conformance record covers a daemon-built per-action 0700 synthetic HOME with only exact 0600 auth/config bytes outside
the model tool namespace; exactly three non-secret helper locator environment values; empty read-only cwd; one
action-pair-only portal, direct or through the pinned `agent-fabric-review-portal-supervisor portal-stdio-v1` Rust
binary whose trusted absolute path/device/inode/digest/code identity and fixed mode are contract-bound; no inherited
provider descriptor, HOME, user/project path, unrelated plugin/source MCP effect, workspace index, shell/write/browser/
general-network effect; outer OS confinement and live canaries; fixed provider transport; and crash-owned
output/capsule/portal cleanup. Unsupported adapter/platform combinations advertise false. The exact activated contract
digest and source mode are stored in each resolved profile slot and route.

Claude/Codex may expose the named portal server/tools directly only after schema/ledger/source-denial/process-cleanup
parity canaries pass; Codex has its own mandatory confinement proof. Otherwise a provider uses the helper when the same
outer isolation can be proved, or advertises false. Cursor/Agy launch only the pinned helper as adapter-internal
bootstrap. Its environment is exactly `AGENT_FABRIC_REVIEW_SOCKET`, `AGENT_FABRIC_REVIEW_ACTION` and
`AGENT_FABRIC_REVIEW_CONTRACT`; all are non-secret locators. It connects to the per-action daemon AF_UNIX broker;
capability stays broker-side. Their model allowlist is exactly mcp(agent-fabric-review-bundle/review_bundle_read) and
mcp(agent-fabric-review-bundle/review_bundle_search). Every other model mcp, command, filesystem, shell,
browser/web/network, resource and prompt effect is denied. Exact-empty list probes remain permitted as above.

The current baseline separates pre-process filesystem intent from process custody so no artifact or child exists without
a durable locator:

~~~sql
review_portal_provider_launch_policies(
  adapter_id NOT NULL, contract_digest NOT NULL,
  launch_policy_json NOT NULL, launch_policy_digest NOT NULL,
  created_at NOT NULL,
  PRIMARY KEY(adapter_id,contract_digest),
  UNIQUE(adapter_id,contract_digest,launch_policy_digest),
  UNIQUE(launch_policy_digest)
)

review_portal_provider_activation_roots(
  daemon_instance_id NOT NULL,
  role NOT NULL CHECK(role IN ('synthetic-home','synthetic-temp')),
  canonical_path NOT NULL, device NOT NULL, inode NOT NULL,
  root_contract_json NOT NULL, root_contract_digest NOT NULL,
  created_at NOT NULL,
  PRIMARY KEY(daemon_instance_id,role),
  UNIQUE(root_contract_digest),
  UNIQUE(daemon_instance_id,role,root_contract_digest)
)

review_portal_provider_launch_source_contract_sets(
  adapter_id NOT NULL, action_id NOT NULL, daemon_instance_id NOT NULL,
  member_count NOT NULL CHECK(member_count >= 1),
  source_contract_set_digest NOT NULL,
  state NOT NULL CHECK(state IN ('building','sealed')),
  revision NOT NULL CHECK(revision IN (1,2)), created_at NOT NULL,
  sealed_at,
  PRIMARY KEY(adapter_id,action_id),
  UNIQUE(source_contract_set_digest),
  UNIQUE(adapter_id,action_id,daemon_instance_id,source_contract_set_digest),
  UNIQUE(adapter_id,action_id,daemon_instance_id,member_count,
    source_contract_set_digest,state),
  CHECK((state='building' AND revision=1 AND sealed_at IS NULL) OR
        (state='sealed' AND revision=2 AND sealed_at IS NOT NULL))
)

review_portal_provider_launch_source_contracts(
  adapter_id NOT NULL, action_id NOT NULL, daemon_instance_id NOT NULL,
  source_contract_set_digest NOT NULL,
  ordinal NOT NULL CHECK(ordinal >= 1),
  source_selector NOT NULL, source_contract_kind NOT NULL CHECK(
    source_contract_kind IN ('effective-configuration-field',
      'activated-executable','action-identity','review-socket',
      'synthetic-home','synthetic-temp','credential-capsule','empty-cwd',
      'policy-stdin-mode','adapter-secret-version')),
  path_class NOT NULL, source_contract_json NOT NULL,
  source_contract_digest NOT NULL, created_at NOT NULL,
  PRIMARY KEY(adapter_id,action_id,ordinal),
  UNIQUE(adapter_id,action_id,source_selector,source_contract_digest),
  UNIQUE(adapter_id,action_id,source_contract_digest),
  UNIQUE(adapter_id,action_id,daemon_instance_id,
    source_contract_set_digest,source_contract_kind,source_contract_digest),
  FOREIGN KEY(adapter_id,action_id,daemon_instance_id,
      source_contract_set_digest)
    REFERENCES review_portal_provider_launch_source_contract_sets(
      adapter_id,action_id,daemon_instance_id,source_contract_set_digest)
)

review_portal_provider_launch_envelopes(
  adapter_id NOT NULL, action_id NOT NULL, contract_digest NOT NULL,
  daemon_instance_id NOT NULL,
  configuration_subject_kind NOT NULL CHECK(
    configuration_subject_kind='provider-action'),
  configuration_id NOT NULL, configuration_revision NOT NULL,
  configuration_digest NOT NULL, effective_configuration_digest NOT NULL,
  executable_identity_digest NOT NULL,
  launch_policy_digest NOT NULL, launch_envelope_json NOT NULL,
  launch_envelope_digest NOT NULL, source_contract_member_count NOT NULL,
  source_contract_set_digest NOT NULL,
  source_contract_set_state NOT NULL CHECK(source_contract_set_state='sealed'),
  created_at NOT NULL,
  PRIMARY KEY(adapter_id,action_id),
  UNIQUE(launch_envelope_digest),
  UNIQUE(adapter_id,action_id,daemon_instance_id,launch_envelope_digest,
    source_contract_set_digest),
  UNIQUE(adapter_id,action_id,configuration_subject_kind,contract_digest,
    configuration_id,configuration_revision,configuration_digest,
    effective_configuration_digest,executable_identity_digest,
    launch_envelope_digest,daemon_instance_id,source_contract_set_digest),
  FOREIGN KEY(adapter_id,action_id,configuration_subject_kind,contract_digest,
      configuration_id,configuration_revision,configuration_digest,
      effective_configuration_digest,executable_identity_digest)
    REFERENCES adapter_effective_configurations(
      subject_action_adapter_id,subject_action_id,subject_kind,
      adapter_contract_digest,configuration_id,configuration_revision,
      configuration_digest,effective_configuration_digest,
      executable_identity_digest),
  FOREIGN KEY(adapter_id,contract_digest,launch_policy_digest)
    REFERENCES review_portal_provider_launch_policies(
      adapter_id,contract_digest,launch_policy_digest),
  FOREIGN KEY(adapter_id,action_id,daemon_instance_id,
      source_contract_member_count,source_contract_set_digest,
      source_contract_set_state)
    REFERENCES review_portal_provider_launch_source_contract_sets(
      adapter_id,action_id,daemon_instance_id,member_count,
      source_contract_set_digest,state)
)

review_portal_provider_exec_closures(
  adapter_id NOT NULL, action_id NOT NULL, contract_digest NOT NULL,
  daemon_instance_id NOT NULL,
  configuration_subject_kind NOT NULL CHECK(
    configuration_subject_kind='provider-action'),
  configuration_id NOT NULL, configuration_revision NOT NULL,
  configuration_digest NOT NULL, effective_configuration_digest NOT NULL,
  executable_identity_digest NOT NULL,
  launch_envelope_digest NOT NULL, source_contract_set_digest NOT NULL,
  provider_closure_json NOT NULL, provider_closure_digest NOT NULL,
  created_at NOT NULL,
  PRIMARY KEY(adapter_id,action_id),
  UNIQUE(adapter_id,action_id,contract_digest,daemon_instance_id,
    provider_closure_digest),
  UNIQUE(adapter_id,action_id,contract_digest,daemon_instance_id,
    provider_closure_digest,launch_envelope_digest,source_contract_set_digest),
  UNIQUE(provider_closure_digest),
  FOREIGN KEY(adapter_id,action_id,configuration_subject_kind,contract_digest,
      configuration_id,configuration_revision,configuration_digest,
      effective_configuration_digest,executable_identity_digest,
      launch_envelope_digest,daemon_instance_id,source_contract_set_digest)
    REFERENCES review_portal_provider_launch_envelopes(
      adapter_id,action_id,configuration_subject_kind,contract_digest,
      configuration_id,configuration_revision,
      configuration_digest,effective_configuration_digest,
      executable_identity_digest,launch_envelope_digest,daemon_instance_id,
      source_contract_set_digest)
)

review_portal_filesystem_directory_name_claims(
  recovery_root_device NOT NULL, recovery_root_inode NOT NULL,
  directory_basename NOT NULL,
  adapter_id NOT NULL, action_id NOT NULL,
  role NOT NULL CHECK(role IN ('custody','claim')),
  PRIMARY KEY(recovery_root_device,recovery_root_inode,directory_basename),
  UNIQUE(adapter_id,action_id,role),
  UNIQUE(adapter_id,action_id,role,recovery_root_device,
    recovery_root_inode,directory_basename)
)

review_portal_action_artifact_name_claims(
  daemon_instance_id NOT NULL,
  artifact_role NOT NULL CHECK(
    artifact_role IN ('synthetic-home','synthetic-temp')),
  activated_root_contract_digest NOT NULL, basename NOT NULL,
  adapter_id NOT NULL, action_id NOT NULL,
  name_role NOT NULL CHECK(name_role IN ('canonical','claim')),
  PRIMARY KEY(activated_root_contract_digest,basename),
  UNIQUE(adapter_id,action_id,artifact_role,name_role),
  UNIQUE(adapter_id,action_id,artifact_role,name_role,daemon_instance_id,
    activated_root_contract_digest,basename),
  FOREIGN KEY(daemon_instance_id,artifact_role,
      activated_root_contract_digest)
    REFERENCES review_portal_provider_activation_roots(
      daemon_instance_id,role,root_contract_digest),
  CHECK(basename NOT IN ('','.','..') AND instr(basename,'/')=0)
)

review_portal_action_artifact_intents(
  adapter_id NOT NULL, action_id NOT NULL, daemon_instance_id NOT NULL,
  role NOT NULL CHECK(role IN ('synthetic-home','synthetic-temp')),
  source_contract_set_digest NOT NULL, source_contract_digest NOT NULL,
  activated_root_contract_digest NOT NULL,
  canonical_path NOT NULL, canonical_basename NOT NULL,
  canonical_path_digest NOT NULL,
  entry_manifest_digest NOT NULL,
  canonical_name_role NOT NULL CHECK(canonical_name_role='canonical'),
  claim_basename NOT NULL,
  claim_name_role NOT NULL CHECK(claim_name_role='claim'),
  artifact_intent_digest NOT NULL, created_at NOT NULL,
  PRIMARY KEY(adapter_id,action_id,role),
  UNIQUE(artifact_intent_digest),
  UNIQUE(activated_root_contract_digest,canonical_path),
  UNIQUE(activated_root_contract_digest,canonical_basename),
  UNIQUE(activated_root_contract_digest,claim_basename),
  UNIQUE(adapter_id,action_id,role,daemon_instance_id,
    source_contract_set_digest,source_contract_digest,
    activated_root_contract_digest,canonical_path,canonical_basename,
    canonical_path_digest,
    entry_manifest_digest,canonical_name_role,claim_basename,
    claim_name_role,artifact_intent_digest),
  FOREIGN KEY(adapter_id,action_id,daemon_instance_id,
      source_contract_set_digest,role,source_contract_digest)
    REFERENCES review_portal_provider_launch_source_contracts(
      adapter_id,action_id,daemon_instance_id,source_contract_set_digest,
      source_contract_kind,source_contract_digest),
  FOREIGN KEY(daemon_instance_id,role,activated_root_contract_digest)
    REFERENCES review_portal_provider_activation_roots(
      daemon_instance_id,role,root_contract_digest),
  FOREIGN KEY(adapter_id,action_id,role,canonical_name_role,
      daemon_instance_id,activated_root_contract_digest,canonical_basename)
    REFERENCES review_portal_action_artifact_name_claims(
      adapter_id,action_id,artifact_role,name_role,daemon_instance_id,
      activated_root_contract_digest,basename),
  FOREIGN KEY(adapter_id,action_id,role,claim_name_role,
      daemon_instance_id,activated_root_contract_digest,claim_basename)
    REFERENCES review_portal_action_artifact_name_claims(
      adapter_id,action_id,artifact_role,name_role,daemon_instance_id,
      activated_root_contract_digest,basename),
  CHECK(canonical_basename <> claim_basename),
  CHECK(canonical_basename NOT IN ('','.','..') AND
    claim_basename NOT IN ('','.','..') AND
    instr(canonical_basename,'/')=0 AND instr(claim_basename,'/')=0)
)

review_portal_action_artifact_states(
  adapter_id NOT NULL, action_id NOT NULL,
  role NOT NULL CHECK(role IN ('synthetic-home','synthetic-temp')),
  artifact_intent_digest NOT NULL,
  phase NOT NULL CHECK(phase IN
    ('reserved','captured','claimed','removed','integrity-failure')),
  capture_kind CHECK(capture_kind IS NULL OR capture_kind IN
    ('complete','partial-recovery')),
  actual_device, actual_inode, actual_link_count,
  actual_entry_manifest_digest, actual_identity_digest,
  cleanup_evidence_digest, revision NOT NULL CHECK(revision >= 1),
  updated_at NOT NULL,
  PRIMARY KEY(adapter_id,action_id,role),
  FOREIGN KEY(adapter_id,action_id,role,artifact_intent_digest)
    REFERENCES review_portal_action_artifact_intents(
      adapter_id,action_id,role,artifact_intent_digest),
  CHECK(
    (phase='reserved' AND capture_kind IS NULL AND
      actual_device IS NULL AND actual_inode IS NULL AND
      actual_link_count IS NULL AND actual_entry_manifest_digest IS NULL AND
      actual_identity_digest IS NULL AND cleanup_evidence_digest IS NULL) OR
    (phase IN ('captured','claimed') AND capture_kind IS NOT NULL AND
      actual_device IS NOT NULL AND
      actual_inode IS NOT NULL AND actual_link_count IS NOT NULL AND
      actual_entry_manifest_digest IS NOT NULL AND
      actual_identity_digest IS NOT NULL AND cleanup_evidence_digest IS NULL) OR
    (phase IN ('removed','integrity-failure') AND
      cleanup_evidence_digest IS NOT NULL)
  )
)

review_portal_filesystem_custody_intents(
  adapter_id NOT NULL, action_id NOT NULL, contract_digest NOT NULL,
  daemon_instance_id NOT NULL,
  recovery_root_path NOT NULL, recovery_root_device NOT NULL,
  recovery_root_inode NOT NULL, recovery_root_identity_digest NOT NULL,
  custody_directory_role NOT NULL CHECK(custody_directory_role='custody'),
  custody_directory_basename NOT NULL,
  custody_directory_contract_digest NOT NULL,
  claim_directory_role NOT NULL CHECK(claim_directory_role='claim'),
  claim_directory_basename NOT NULL,
  socket_basename NOT NULL, capsule_basename NOT NULL,
  expected_capsule_content_digest NOT NULL,
  provider_closure_digest NOT NULL, launch_envelope_digest NOT NULL,
  source_contract_set_digest NOT NULL, launch_nonce_digest NOT NULL,
  home_artifact_role NOT NULL CHECK(home_artifact_role='synthetic-home'),
  home_artifact_intent_digest NOT NULL,
  temp_artifact_role NOT NULL CHECK(temp_artifact_role='synthetic-temp'),
  temp_artifact_intent_digest NOT NULL,
  claim_name_codec NOT NULL CHECK(
    claim_name_codec='agent-fabric-custody-claim-v1'),
  intent_digest NOT NULL, created_at NOT NULL,
  PRIMARY KEY(adapter_id,action_id),
  UNIQUE(launch_nonce_digest),
  UNIQUE(adapter_id,action_id,intent_digest),
  UNIQUE(adapter_id,action_id,intent_digest,contract_digest,daemon_instance_id,
    provider_closure_digest,launch_envelope_digest,source_contract_set_digest,
    launch_nonce_digest,home_artifact_role,home_artifact_intent_digest,
    temp_artifact_role,temp_artifact_intent_digest,
    recovery_root_path,recovery_root_device,recovery_root_inode,
    recovery_root_identity_digest,custody_directory_basename,
    custody_directory_contract_digest,claim_directory_basename,
    socket_basename,capsule_basename,expected_capsule_content_digest,
    claim_name_codec),
  FOREIGN KEY(adapter_id,action_id,custody_directory_role,
      recovery_root_device,recovery_root_inode,custody_directory_basename)
    REFERENCES review_portal_filesystem_directory_name_claims(
      adapter_id,action_id,role,recovery_root_device,recovery_root_inode,
      directory_basename),
  FOREIGN KEY(adapter_id,action_id,contract_digest,daemon_instance_id,
      provider_closure_digest,launch_envelope_digest,source_contract_set_digest)
    REFERENCES review_portal_provider_exec_closures(
      adapter_id,action_id,contract_digest,daemon_instance_id,
      provider_closure_digest,launch_envelope_digest,
      source_contract_set_digest),
  FOREIGN KEY(adapter_id,action_id,home_artifact_role,
      home_artifact_intent_digest)
    REFERENCES review_portal_action_artifact_intents(
      adapter_id,action_id,role,artifact_intent_digest),
  FOREIGN KEY(adapter_id,action_id,temp_artifact_role,
      temp_artifact_intent_digest)
    REFERENCES review_portal_action_artifact_intents(
      adapter_id,action_id,role,artifact_intent_digest),
  FOREIGN KEY(adapter_id,action_id,claim_directory_role,
      recovery_root_device,recovery_root_inode,claim_directory_basename)
    REFERENCES review_portal_filesystem_directory_name_claims(
      adapter_id,action_id,role,recovery_root_device,recovery_root_inode,
      directory_basename),
  CHECK(substr(recovery_root_path,1,1)='/'),
  CHECK(custody_directory_basename <> claim_directory_basename),
  CHECK(socket_basename <> capsule_basename),
  CHECK(instr(custody_directory_basename,'/')=0 AND
    instr(claim_directory_basename,'/')=0 AND
    instr(socket_basename,'/')=0 AND instr(capsule_basename,'/')=0),
  CHECK(custody_directory_basename NOT IN ('','.','..') AND
    claim_directory_basename NOT IN ('','.','..') AND
    socket_basename NOT IN ('','.','..') AND
    capsule_basename NOT IN ('','.','..'))
)

review_portal_filesystem_custody_state(
  adapter_id NOT NULL, action_id NOT NULL,
  state NOT NULL CHECK(state IN
    ('open','cleaned','integrity-failure')),
  revision NOT NULL CHECK(revision >= 1), cleanup_evidence_digest,
  updated_at NOT NULL,
  PRIMARY KEY(adapter_id,action_id),
  FOREIGN KEY(adapter_id,action_id)
    REFERENCES review_portal_filesystem_custody_intents(adapter_id,action_id),
  CHECK((state IN ('cleaned','integrity-failure')) =
    (cleanup_evidence_digest IS NOT NULL))
)

review_portal_process_custody(
  adapter_id NOT NULL, action_id NOT NULL, contract_digest NOT NULL,
  daemon_instance_id NOT NULL, filesystem_intent_digest NOT NULL,
  launch_nonce_digest NOT NULL, launch_action_binding_digest NOT NULL,
  launch_registration_digest NOT NULL,
  process_custody_launch_digest NOT NULL, launch_ack_digest NOT NULL,
  launch_row_revision NOT NULL CHECK(launch_row_revision=1),
  supervisor_pid NOT NULL CHECK(supervisor_pid > 0),
  supervisor_start_time NOT NULL CHECK(supervisor_start_time > 0),
  provider_root_pid NOT NULL CHECK(provider_root_pid > 0),
  provider_root_start_time NOT NULL CHECK(provider_root_start_time > 0),
  process_group_id NOT NULL CHECK(process_group_id > 0),
  session_id NOT NULL CHECK(session_id > 0),
  supervisor_executable_identity_digest NOT NULL,
  launch_stub_identity_digest NOT NULL, provider_closure_digest NOT NULL,
  launch_envelope_digest NOT NULL, source_contract_set_digest NOT NULL,
  home_artifact_role NOT NULL CHECK(home_artifact_role='synthetic-home'),
  home_artifact_intent_digest NOT NULL,
  temp_artifact_role NOT NULL CHECK(temp_artifact_role='synthetic-temp'),
  temp_artifact_intent_digest NOT NULL,
  ancestry_manifest_digest NOT NULL,
  recovery_root_path NOT NULL, recovery_root_device NOT NULL,
  recovery_root_inode NOT NULL, recovery_root_identity_digest NOT NULL,
  custody_directory_basename NOT NULL,
  custody_directory_contract_digest NOT NULL,
  claim_directory_basename NOT NULL,
  custody_directory_device NOT NULL, custody_directory_inode NOT NULL,
  claim_directory_device NOT NULL,
  claim_directory_inode NOT NULL,
  claim_name_codec NOT NULL CHECK(
    claim_name_codec='agent-fabric-custody-claim-v1'),
  socket_basename NOT NULL, socket_claim_basename NOT NULL,
  socket_file_device NOT NULL, socket_file_inode NOT NULL,
  socket_link_count NOT NULL CHECK(socket_link_count=1),
  socket_identity_digest NOT NULL,
  socket_cleanup_state NOT NULL,
  capsule_basename NOT NULL, capsule_claim_basename NOT NULL,
  capsule_file_device NOT NULL, capsule_file_inode NOT NULL,
  capsule_link_count NOT NULL CHECK(capsule_link_count=1),
  capsule_content_digest NOT NULL, capsule_cleanup_state NOT NULL,
  control_fd_number NOT NULL CHECK(control_fd_number=3),
  registration_fd_number NOT NULL CHECK(registration_fd_number=4),
  provider_exec_fd_number NOT NULL CHECK(provider_exec_fd_number=5),
  provider_cwd_fd_number NOT NULL CHECK(provider_cwd_fd_number=6),
  executable_parent_fd_number NOT NULL CHECK(executable_parent_fd_number=7),
  connection_state NOT NULL CHECK(
    connection_state IN ('waiting','consumed','closed')),
  process_state NOT NULL CHECK(process_state IN
    ('preparing','running','terminating','cleaned','integrity-failure')),
  directory_cleanup_state NOT NULL,
  directory_cleanup_evidence_digest,
  cleanup_generation NOT NULL CHECK(cleanup_generation >= 0),
  cleanup_evidence_digest, revision NOT NULL CHECK(revision >= 1),
  created_at NOT NULL, updated_at NOT NULL,
  PRIMARY KEY(adapter_id, action_id),
  UNIQUE(launch_nonce_digest),
  FOREIGN KEY(adapter_id,action_id,filesystem_intent_digest,
      contract_digest,daemon_instance_id,provider_closure_digest,
      launch_envelope_digest,source_contract_set_digest,launch_nonce_digest,
      home_artifact_role,home_artifact_intent_digest,
      temp_artifact_role,temp_artifact_intent_digest,
      recovery_root_path,recovery_root_device,recovery_root_inode,
      recovery_root_identity_digest,custody_directory_basename,
      custody_directory_contract_digest,claim_directory_basename,
      socket_basename,capsule_basename,capsule_content_digest,
      claim_name_codec)
    REFERENCES review_portal_filesystem_custody_intents(
      adapter_id,action_id,intent_digest,
      contract_digest,daemon_instance_id,provider_closure_digest,
      launch_envelope_digest,source_contract_set_digest,launch_nonce_digest,
      home_artifact_role,home_artifact_intent_digest,
      temp_artifact_role,temp_artifact_intent_digest,
      recovery_root_path,recovery_root_device,recovery_root_inode,
      recovery_root_identity_digest,custody_directory_basename,
      custody_directory_contract_digest,claim_directory_basename,
      socket_basename,capsule_basename,expected_capsule_content_digest,
      claim_name_codec),
  CHECK(claim_directory_basename <> custody_directory_basename),
  CHECK(claim_directory_device = custody_directory_device),
  CHECK(claim_directory_inode <> custody_directory_inode),
  CHECK(socket_basename <> capsule_basename),
  CHECK(socket_claim_basename <> capsule_claim_basename),
  CHECK(socket_claim_basename NOT IN (socket_basename,capsule_basename)),
  CHECK(capsule_claim_basename NOT IN (socket_basename,capsule_basename)),
  CHECK(instr(socket_basename,'/')=0 AND
    instr(socket_claim_basename,'/')=0 AND
    instr(capsule_basename,'/')=0 AND
    instr(capsule_claim_basename,'/')=0),
  CHECK(socket_basename NOT IN ('','.','..') AND
    socket_claim_basename NOT IN ('','.','..') AND
    capsule_basename NOT IN ('','.','..') AND
    capsule_claim_basename NOT IN ('','.','..')),
  CHECK(socket_cleanup_state IN
    ('canonical','claimed','removed','integrity-failure')),
  CHECK(capsule_cleanup_state IN
    ('canonical','claimed','removed','integrity-failure')),
  CHECK(directory_cleanup_state IN
    ('active','children-removed','canonical-removed','removed',
     'integrity-failure')),
  CHECK((directory_cleanup_state='active') =
    (directory_cleanup_evidence_digest IS NULL)),
  CHECK(directory_cleanup_state NOT IN
    ('children-removed','canonical-removed','removed') OR
    (socket_cleanup_state='removed' AND capsule_cleanup_state='removed')),
  CHECK(process_state <> 'cleaned' OR directory_cleanup_state='removed'),
  CHECK(directory_cleanup_state <> 'removed' OR
    process_state IN ('cleaned','integrity-failure')),
  CHECK((process_state IN ('cleaned','integrity-failure')) =
    (cleanup_evidence_digest IS NOT NULL))
)
~~~

All displayed locator/identity path, device, inode, basename and kind-specific digest fields are nonnull and immutable.
Phase evidence digests are null only in their declared pre-evidence state and become immutable nonnull values in the
owning state CAS. Before any per-action HOME/temp directory, custody/claim directory, filesystem portal socket, capsule
or process exists, one transaction reserves their four role/name claims, inserts the exact HOME/temp artifact intents
and their `reserved` states, reserves both globally unique recovery-root child names and inserts the immutable
filesystem intent plus `open` state. `open` with no process row is the reserved arm; `open` with its exact process row
is the process-bound arm. Process-row existence, not a separately mutable flag, is the atomic ownership transition.
Daemon-created anonymous stdio pipes/socketpairs or an OS-owned PTY may be captured for the closure before that
transaction only while every endpoint remains in the daemon, no child exists and no project/provider namespace entry is
created. Transaction failure closes them; daemon death lets the kernel close them, leaving no recoverable path or
external effect. The HOME/temp, listener path, capsule and custody directories remain strictly post-intent. Exactly two
role-distinct name claims must join each intent; orphan, missing, crossed or post-insert-mutated claims are rejected,
and neither claim is reused while its immutable intent remains registered. It binds an already-opened 0700 daemon
recovery root by path/device/inode plus all create-exclusive relative basenames and expected capsule digest. The daemon
then creates the canonical and distinct 0700 claim directories only beneath that no-follow root, writes/binds each
artifact and fsyncs every file/directory/ parent before launch. A crash while reserved can see absent or partially
created objects but no provider has executed; recovery uses the exact root/intent and the same trusted-claim
revalidation, removes only a proved daemon-created object, fsyncs the root after each removal and CASes the
open/no-process state to cleaned. It permits only the two reserved recovery-root directory basenames/their two declared
children plus the exact HOME/temp paths/manifests named by the two artifact intents; any extra, crossed or substituted
object records integrity failure without deletion. Fully captured identities, contract and daemon instance are
equality-copied through the displayed composite FK when the process row is inserted in the pre-ACK transaction. That row
is nondeletable and its identity fields are immutable. Only it then owns live cleanup; state becomes cleaned or
integrity-failed only after the matching process/directory and both action-artifact terminals, and is never a second
owner. Direct-SQL fixtures reject process insertion against non-open state, crossed
intent/contract/daemon/root/name/content, a process-less process-bound claim and delete/reversion. It provisions both
directories on the same filesystem while sharing neither inode nor basename, and current-build activation probes
same-mount atomic no-replace rename plus provider denial of read/list/write access to the claim namespace. The row also
persists both claim basenames and `claim_name_codec=agent-fabric-custody-claim-v1`. For each entry the claim name is
`.agent-fabric-claim-` plus lowercase hex SHA-256 of the ASCII bytes `agent-fabric-custody-claim-v1` followed by one
`0x00` byte, then the canonical- basename UTF-8 bytes, u64be(device), u64be(inode), one kind byte (`0x00` socket, `0x01`
regular file) and the raw 32 digest bytes, concatenated in that order. The Rust boundary recomputes and equality-checks
the persisted name. Admission rejects either claim name matching any canonical name or the other claim name. Thus
executable upgrade cannot silently change a live record's locator. `socket_identity_digest` is `sha256:` plus lowercase
SHA-256 of the ASCII bytes `agent-fabric-custody-socket-v1` followed by one `0x00` byte, then `u64be(device) ||
u64be(inode)`. The entry must be `S_IFSOCK`; `capsule_content_digest` is `sha256:` plus lowercase SHA-256 of the exact
bounded regular-file bytes and the persisted device/inode must also match. Both persisted link counts are exactly one.
Golden vectors pin both exact domain preimages, the socket digest and both claim names across Rust and TypeScript. No
socket-content digest exists. Failure refuses launch and leaves capability false. These private crash-locating fields
never cross internal boundaries; only their nonsecret correlation digests may do so, and none is public/model-visible.

The HOME/temp artifact intent is independently reproducible. `artifact_intent_digest` uses domain
`agent-fabric-portal-action-artifact-intent-v1`, `0x00` and JCS of every immutable intent-row field except that digest
and `created_at`. Its claim name is `.agent-fabric-action-claim-` plus lowercase SHA-256 hex of the ASCII bytes
`agent-fabric-portal-action-artifact-claim-v1`, `0x00`, role UTF-8, one `0x00`, canonical-path-digest raw bytes,
activated-root-contract-digest raw bytes and source-contract-digest raw bytes. Home and temp claim names are distinct
direct siblings of their canonical action directory beneath the exact same cited activated root. The root is never
exposed; outer confinement grants the provider only its canonical child and current-build canaries deny parent/claim
lookup, open and mutation. Same-root sibling rename supplies the proved atomic same-filesystem boundary without an
unstored claim-root locator.
