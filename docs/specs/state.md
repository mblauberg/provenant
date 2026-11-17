# State and config

Owner: `crates/core` (IDs, roots, time), `crates/records` (codecs, digests),
`crates/config` (config, release, managed install), `crates/sqlite` (SQLite formats),
`crates/cli` (provider preparation) and `crates/fabric-runtime` (peer status projections)
Anchor: `crates/core/src/roots.rs`, `crates/records/src/lib.rs`,
`crates/config/src/managed.rs`, `crates/cli/src/run/provider/preparation.rs`,
`crates/fabric-runtime/src/runs.rs`, `src/fabric/peer.rs`

Keys and defaults: `provenant config schema`. Operator steps: the
[install](../runbooks/install.md) and [GC](../runbooks/gc.md) runbooks.

## Roots and identities

### Root selection never falls back

An empty or relative explicit root fails and never yields to the default. A
root inside a declared credential, Git-metadata or other-state boundary is
refused. A root that contains an other-state boundary is refused too, but one
that contains Git metadata, a credential store or an advisory location
proceeds, because lane checkouts, provider homes and installed releases live
under the state root. A root inside a checkout, a workspace or the retired
agent-harness location warns with its source and proceeds. OS binding captures existing Git markers and
linked-worktree metadata; installed release directories imply no Git boundary.
Credential boundaries use the same concrete, sourced declarations as lane
grant validation; a credential directory name elsewhere in a path establishes
no boundary.
The same declaration and decision apply before
and after alias binding. Existing workspace `.agent-run` stores are captured
as other-state boundaries; that name alone establishes no boundary.
Reads create no state.
- Why: a wrong root mixes or destroys another installation's state.
- Owner: `crates/core/src/roots.rs`, `src/roots.rs`

### The first install uses a fresh root

The first 2.0 install validates a sibling staging root against the captured
install root inputs, changing only the state candidate. It validates the
release and every staged target before stopping 1.x agents or publishing the
new root. Into an absent or empty default root it copies only owner
configuration (`config.toml` and `host-id`); 1.x state is not migrated, and
install never reads a 1.x install receipt. A fresh custom root remains valid.
If the selected root is identified as 1.x by its default v1 path or old-root
markers and no handoff applies, install reports `STATE_FORMAT_BREAK`, names the
old root, and directs the operator to the default v2 root for owner-config
handoff or to a fresh custom root with configuration selected manually.
The old root remains for the operator to inspect and remove after preserving
anything needed.

Install owns only its declared Provenant artifact names. It replaces or removes
those names directly, including obsolete named LaunchAgents, without reading a
1.x receipt. Unrecognised state and client configuration remain outside that
ownership set; release notes give any required manual cleanup, and doctor may
warn about recognised leftovers.
- Why: the new root starts with one state format and install cannot infer
  ownership from a legacy receipt.
- Owner: `src/state_handoff.rs`, `src/install.rs`, `crates/core/src/roots.rs`

### Identifiers have one spelling

A value that can normalise two ways is rejected at the wire boundary.
- Why: two spellings let one record alias another.
- Owner: `crates/core/src/`

### Graph names retain one identity

A graph name identifies one graph generation for its project's lifetime.
Checkpoint collection leaves a durable name reservation because retained
attempt, chair and event records continue to refer to the graph by name. A
collected name is not allocated to a later graph.
- Why: reusing the name can collide with retained attempt keys or inherit a
  stale chair association.
- Owner: `crates/fabric-runtime/src/graph/journal.rs`,
  `src/fabric/interface.rs`

## SQLite stores

### A format major permits additive evolution

Each SQLite store has a fixed `user_version` base identifier within its
containing state-root format major. The numeric identifier is local to that
store and need not equal the root major; it is fixed for the life of that root
major and cannot be bumped independently. Binaries for the same root major can
read and write the store regardless of release minor. Each owner declares one
ordered list of additive steps, tagged with the release minor that added them;
no minor counter is stored. Steps add tables and indexes with `IF NOT EXISTS`,
or columns that are nullable or have a non-NULL constant default. They never
rewrite, drop or rename existing objects. Every insert names its columns, so
an older writer ignores columns added by a newer one.

A writable open creates the base and any missing steps atomically, or applies
missing steps to an existing same-major store. Existence determines which
steps are missing. A current store opens without a write; concurrent openers
re-check under the same immediate transaction before applying steps. A reader
ignores unknown objects, but refuses a missing known base table or a missing
declared step. A missing step requires a writable open. A different root major
is refused without changing the store. The sole bounded exception to the
no-older-shape reader or migration rule is these additive SQLite steps within
the same root major. A non-additive stored change requires a new root format
major and a fresh root, with the owner-configuration handoff used for 1.x to
2.0.
- Why: additive changes within one root major must preserve stored state and permit
  binaries of different minors to share the same format.
- Owner: `crates/sqlite/src/schema.rs`, each store's schema declaration

## Records

### Commit before signal

Owners commit durable state before the lossy change-generation signal; a
failed signal never fails the committed write. Readers treat an absent or reset
generation as unknown and re-read.
- Why: a hint must not become a second source of truth.
- Owner: `crates/os/src/changes.rs`

### Strict, bounded records

Unsupported versions fail before mutation. The reader rejects ambiguous or
unbounded input and never echoes the rejected value. Closed codecs reject or
discard unknown fields; records with explicit extension maps preserve their
extensions. Host-owned semantics belong to typed fields on their owning record;
malformed values fail decoding instead of becoming absent facts. Extension maps
preserve only unknown fields, so a newer writer's data can survive a round trip.
Edge-built facts are `Serialize` only.
- Why: a forged file must not become evidence.
- Lint: `scripts/check-extra-fields` rejects host literal and constant keys in
  non-test extension-map reads and writes, with no allowlist.
- Owner: `crates/records/src/`

Landing records retain the current snapshot and consumer revisions without an unconsumed event outbox. Their readers use the common bounded records decoder and reject removed fields.

### One digest preimage

`CanonicalV1` bytes are deterministic and domain-framed; it is not RFC 8785.
- Why: a digest is an identity only if every writer computes the same bytes.
- Owner: `crates/records/src/canonical.rs`

## Configuration

### Config never widens authority

Syntax, version and filesystem-boundary failures refuse loading; an invalid
optional value warns and uses the default, and a malformed restriction never
becomes permission. `config set` publishes atomically and keeps unknown keys;
`config show` redacts secrets.
Project binding directories must be directories owned by the current user and
private from group and other users. Permission or ownership failures have a
local repair diagnostic; malformed bindings and filesystem errors retain their
own causes, and lookup never repairs state.
Callers that surface the typed unsafe-directory failure preserve its safe local
location and repair action. Local attempt status retains that detail, while peer
status and events use the cause on their terminal state to replace terminal
text with a safe reason and remedy. The event projection uses its own recorded
cause, not a later attempt lookup. Direct diagnostic projections disclose remote
paths only through an approved anchor.
Commands load their declared domains; invalid unused domains warn, and effective defaults retain their compiled source.
One sourced port table declares forge operations, verify, land, CI observation, worktree layout, naming, post-create, tracker, release, attempt admission and build admission backends; repository values at `.provenant/ports.json` override host `state/ports.json`, then compiled builtin defaults. Each explicitly replaced port takes its source from the containing layer before decoding; a serialised source is optional and cannot override that provenance. Omitted ports retain their lower layer and source. This repository stores its source at `config/ports.json` and links the optional Provenant path there; CI reads the source directly. Verify tiers declare free-form names, an explicit default, selection and host-load policy, while the builtin layout declares its roots and branch path once.
Forge operations share one typed port and one Builtin/Command/None adapter in `config::ports`; CI observation and tracker select their sourced category through that same adapter. The land owner resolves `PortTable.land` before publication effects, independently of candidate and ordering strategy. The resolved publication declaration is sealed in the landing plan. The land command reuses `records::landing::RepositoryCommand`: exit 0 means landed, other nonzero exits refuse except its declared unknown code. A command port inherits only the declared execution-context environment (paths, home, temporary directory, locale and toolchain selection), plus explicit owner-supplied values; ambient credential and agent variables are not forwarded. A command port runs configured argv directly, followed by owner-supplied arguments, with no shell; declared exit codes select succeeded, unknown or refused. Consumers of response bytes treat incomplete capture as unavailable; exit-only admission uses declared exit success and records capture completeness separately. `none` disables that port without selecting the builtin behavior.
Naming commands receive the requested branch and project path as arguments; complete single-line branch output derives a name. Empty output keeps the request; invalid output or a command failure warns and keeps it. Post-create commands receive the branch and new worktree path. Admission commands receive `PROVENANT_PORT_PHASE=acquire|release`, `PROVENANT_ADMISSION_ATTEMPT`, `PROVENANT_ADMISSION_EPOCH`, `PROVENANT_ADMISSION_WORKTREE`, `PROVENANT_ADMISSION_PROJECT` and an informational `PROVENANT_ADMISSION_OWNER_PID`. The repository owns its semaphore keyed by attempt and epoch: acquire and release are idempotent, and release fences any late acquire for the same epoch. A queued retry uses a new epoch. The existing `Starting` claim and launcher ownership cover acquisition and provider preparation, so cancellation retains admission until preparation unwinds. Provenant freezes the exact command/source privately before acquisition and invokes release outside terminal transaction locks. A matching successful command receipt proves release after terminal settlement without changing the immutable attempt record. Busy acquisition returns the attempt for retry after release is known, without a Provenant semaphore.
Build admission resolves independently of attempt leases. Its builtin adapter uses host build slots and disk policy; a command adapter streams a repository lock wrapper with argv followed by the real Cargo executable and its original arguments. The wrapper receives the shared command context environment, explicit Cargo/Rust build knobs and `PROVENANT_PORT_SOURCE`; it needs no writable host-state receipt. `none` performs no build admission. The separate explicit local-Cargo execution policy still applies to command and disabled admission backends.
CI observation commands receive `PROVENANT_PORT_REQUEST_PATH` and `PROVENANT_PORT_IF_NONE_MATCH` (empty when no validator is sent), and emit `{"status": number, "headers": {"etag": "..."}, "body": "UTF-8 response bytes"}`; 304 responses use an empty body. Missing builtin response headers are a typed observation: the body remains readable, no ETag is retained, and later reads fetch the full response. Repository discovery uses the same selected backend with the literal request path `repos/{owner}/{repo}`; its body is the UTF-8 JSON object `{"full_name":"owner/repo"}` and `none` refuses the lookup without using the builtin adapter. Verify command ports substitute `{tier}` in their declared argv and receive `PROVENANT_VERIFY_TIER`, `PROVENANT_VERIFY_PROJECT_ROOT` and the private `PROVENANT_VERIFY_EFFECTIVE_CONFIG_JSON`, which contains only the resolved verify declaration. The builtin engine uses that declaration so host overrides apply; direct repository CI still reads `config/ports.json` without Provenant. Tracker commands receive one issue number and emit the normalised projection `{body, state, kind}`. Release commands receive optional `--base` and `--head`, then emit the complete `{findings: [{rule, path, line, message}]}` set for Provenant to page; `path` uses the existing `PathWire` representation and `line` is one-based. Command receipts under `port-receipts/<owner>/` bind typed owner/backend, command digest, source, phase and hashed request key to outcome, exit code, elapsed time and stdout digest; these versioned, bounded records omit argv, environment and output contents. Visible publication with unconfirmed durability remains a typed visible receipt, never an absent receipt.
- Why: errors need visible remedies without expanding authority.
- Owner: `crates/config/src/`

### Retention keeps evidence

GC retains evidence and unimported Git work; it expires resumable provider state
only after archiving history. It skips busy items, protects active reservations
and continuations, and uses identity-checked deletion without following links.
Usage retention belongs to [telemetry](telemetry.md#the-usage-ledger-outlives-the-run-store).
`RETENTION_CLASSES` is the executable declaration of every retention class: a typed owner and adapter, triggers, eligibility and effective schema settings. Required collection and measurement methods are generated from that declaration; there is no wildcard adapter. Scheduled tick and explicit GC execute the same registry path. Each scheduled class runs its own adapter, including payload cap, attempt scratch, graph checkpoints and port receipts; byte rows come from adapter measurements, with complete, partial or unavailable observations and visible collection failures.
Admission command receipts become eligible only after their attempt is terminal and release is proven. Naming and post-create command receipts carry a declared age lifetime, defaulting to 30 days and stored on each receipt. GC resumes interrupted checkpoint collection under the graph lock when a reserved graph name has no checkpoint; remaining prompt blobs, sidecars and pending markers are collected while the name reservation survives.
Removal records preserve subject, tip, subsumed or landed predicate and evidence source; shallow history, conflicted merge trees, offline forge and rewritten default branches remain typed kept outcomes.
- Why: freeing space must not destroy evidence or a session in use.
- Owner: `crates/config/src/gc.rs`; `worktrees.rs`; `crates/run/src/retention.rs`

## Release identity

### The source digest is the binary's identity

It covers shipped source, lockfile, toolchain, packager and bundled assets, with
no override; test-only edits do not change it.
- Why: the installed binary must be provably the reviewed source.
- Owner: `build.rs`, `crates/records/src/source.rs`

### Releases are immutable

Install verifies and smoke-tests a release before trusting it. `release.json`
is published last and a published release is never repaired. A digest proves
identity, not publisher trust.
- Why: a partial or swapped release must never run.
- Owner: `crates/config/src/release.rs`

## Managed install

### Harness integrations have one declaration

Managed harness paths and native hook event mappings come from `AdapterSpec`.
Every supported instruction target links the same release instruction asset.
An unavailable integration names its reason and source in install findings.
Provenant has no MCP integration: the CLI is the agent interface. Install
removes retired 1.x Fabric MCP registrations by their declared names and leaves
every other MCP server untouched.
- Why: adapters share capability intent while native formats remain replaceable.
- Owner: `crates/records/src/adapters.rs`, `crates/config/src/harness.rs`

### Preview before every write

A failed preview prevents writes, and `--dry-run` creates no root, lock or
receipt. Identity and bytes are rechecked before each publication; drift stops
the install.
- Why: the user's harness config changes only in view.
- Owner: `crates/config/src/managed.rs`

### Only proven-owned entries change

Removal unlinks only entries whose value still matches the receipt, never
recursively; adoption does not authorise deletion. After a home or
state-root move, stale entries are reported without their paths being inspected.
- Why: an old path may now belong to something else.
- Owner: `crates/config/src/managed.rs`

### Foreign bytes and user hooks survive

Edits replace only owned values, keeping comments and order; malformed or
symlinked configs are refused unchanged. Edited hooks are not overwritten.
- Why: a harness config holds the user's other tools and secrets.
- Owner: `crates/config/src/splice.rs`, `crates/config/src/hooks.rs`

### LaunchAgents are owned like any entry

The peer-agent and tick plists join the preview and receipt. A symlink at the
plist or a parent is refused, and an older plist is adopted only on an exact
byte match.
- Why: a LaunchAgent runs unattended with the user's authority.
- Owner: `crates/config/src/managed.rs`, `src/tick_agent.rs`

### Re-apply is the recovery

A partial install lists applied and remaining actions, and rerunning `install`
recovers it, even with no receipt. There are no rollback journals.
- Why: one idempotent path is easier to trust than a rollback engine.
- Owner: `crates/config/src/managed.rs`

## Typed host metadata in format 2

`Trace.telemetry_context` owns launch-time `TelemetryContext` observations.
Capture passes the declared value directly into the trace; readers never decode
it from an extension map. Port receipt retention reads the attempt's declared
`repository_admission_binding`, including its validated command digest and lease
epoch, rather than maintaining a second admission-binding wire shape.
Known fields with the wrong JSON type fail record decoding. Missing optional
observations remain unknown. Unknown forward fields alone remain in extension
maps; there are no old-shape readers or key aliases.
- Why: the 2.0 stored shape gives each host fact one shared writer/reader type.
- Owner: `crates/records/src/trace.rs`; `crates/records/src/run.rs`;
  `crates/fabric-runtime/src/graph/model.rs`; `crates/fabric-runtime/src/coordination/facade.rs`;
  `crates/fabric-runtime/src/runs.rs`

Sandbox mode and sandbox-admission receipts are declared route metadata. The
confinement writer, launch writer and attempt readers share those typed fields;
no shared string key or extension-map decode determines sandbox authority.
Preparation failures, guard evidence, launch preparation observations and usage
aggregation likewise belong to their declared run record owners. Optional facts
may be absent; a present value with the wrong type fails decoding.

Graph specifications, attempts and routes own provisioning, placement, review,
peer and template metadata. Relay and interface callers pass the declared values
directly. Peer output carries a typed planner-fallback reason, while its typed
attempt target owns identity. Wait replies carry a typed observation report.
Telemetry reads declared work-context PR, lens, task and risk fields. Every new
persisted field is optional or defaulted; flattened extension maps retain
unknown fields from newer minor writers without interpreting them as host facts.
