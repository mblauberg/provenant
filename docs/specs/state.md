# State and config

Owner: `crates/core` (IDs, roots, time), `crates/records` (codecs, digests),
`crates/config` (config, release, managed install), `crates/sqlite` (SQLite formats),
`crates/cli` (provider preparation) and `crates/fabric-runtime` (peer status projections)
Anchor: `crates/core/src/roots.rs`, `crates/records/src/lib.rs`,
`crates/config/src/managed.rs`, `crates/cli/src/run/provider/preparation.rs`,
`crates/fabric-runtime/src/runs.rs`, `src/fabric/peer.rs`

Keys and defaults: `provenant config schema`. Environment variables:
[consumer contract](consumer-contract.md#environment). Operator steps: the
[install](../runbooks/install.md), [configuration](../runbooks/configuration.md)
and [GC](../runbooks/gc.md) runbooks.

## Roots and identities

### Root selection never falls back

An empty or relative explicit root fails and never yields to the default. A root
inside a declared credential, Git-metadata or other-state boundary is refused,
as is one containing an other-state boundary. A root that contains Git metadata,
a credential store or an advisory location proceeds, because lane checkouts,
provider homes and installed releases live under the state root. A root inside a
checkout or workspace warns with its source and proceeds. Credential boundaries
use the same concrete, sourced declarations as lane grant validation, before and
after alias binding; a credential directory name elsewhere in a path is no
boundary. Reads create no state.
- Why: a wrong root mixes or destroys another installation's state.
- Owner: `crates/core/src/roots.rs`, `src/roots.rs`

### The first install uses a fresh root

The first 2.0 install copies only owner configuration into an absent or empty
default root, never migrates 1.x state and never writes the old root; a selected
root with the 1.x layout is refused with `STATE_FORMAT_BREAK`. Install owns only
its declared Provenant artifact names and never reads a 1.x receipt. Procedure:
[first 2.0 install after 1.x](../runbooks/install.md#migrating-from-1x).
- Why: install cannot infer ownership from a receipt of another state format.
- Owner: `src/state_handoff.rs`, `src/install.rs`, `crates/core/src/roots.rs`

### Identifiers have one spelling

A value that can normalise two ways is rejected at the wire boundary.
- Why: two spellings let one record alias another.
- Owner: `crates/core/src/`

### Host-project state has one key

Host-project state lives only in the ProjectId binding directory
`<state>/projects/<ProjectId>`. The port layer, config overlays, admission
classes and GitHub policy resolve that directory through
`project_binding::host_project_dir` or `project_binding_dir`; the obsolete
64-hex `project_key` directory is never read. Upgrade moves settings filed
there with the `host-project-dirs` install migration: it moves a directory only
when the binding directory is absent, and a conflict warns with both paths and
leaves both in place.
- Why: two directory keys split one project's settings and silently hide the
  layer a user believes they configured.
- Owner: `crates/config/src/project_binding.rs`, `crates/config/src/overlays.rs`,
  `crates/config/src/admission.rs`, `src/install/migrations.rs`

### Wall time has one stated bound

`WallTime::MAX` is the single greatest instant the wire accepts. A consumer that
needs the latest representable instant, including the graph checkpoint
envelope, derives it from the constant and has no epoch fallback.
- Why: a bound naming a value the constructor rejects invites silent fallbacks
  that hide the mismatch.
- Owner: `crates/core/src/time.rs`, `crates/fabric-runtime/src/graph/envelope.rs`

### Graph names retain one identity

A graph name identifies one graph generation for its project's lifetime.
Checkpoint collection leaves a durable name reservation because retained
attempt, chair and event records still refer to the graph by name; a collected
name is never allocated to a later graph.
- Why: reuse can collide with retained attempt keys or inherit a stale chair
  association.
- Owner: `crates/fabric-runtime/src/graph/journal.rs`, `src/fabric/interface.rs`

### Graph checkpoints cover persisted text

Admission normalises prompt text before planning or digesting, and result and
patch ingress use the same normal form. `FileJournal` hashes the typed
checkpoint, externalises text without changing its bytes and validates the
reconstructed result before atomic publication; readers see one complete
published record. Every corrupt checkpoint stays visible as unreadable through
status with its graph ID and typed reason, and landing review reports unreadable
graph sources by graph ID while keeping valid settlements. The first tick read of
a corrupt checkpoint records its file digest and reason in a quarantine marker;
identical bytes are skipped on later ticks, changed bytes are reported again and
`provenant stop -- NAME` retires the record. Pending-index recovery never uses
unverified lifecycle fields to classify a corrupt record as settled.
- Why: stored text must verify on reload, and one damaged graph must not hide
  another's review evidence or flood every tick with an unresolvable retry.
- Owner: `crates/fabric-runtime/src/graph/journal.rs`, `src/pr/landing/adapters.rs`

`GraphDocument.subject` is additive: the host writes the admitted `PinnedSubject`
at admission and overwrites any caller-supplied value, and its serialization is
skipped when absent so older execution digests stay byte-identical. There is no
`CHECKPOINT_SCHEMA_VERSION` bump. `GraphCheckpoint.subject` is a serde-skipped
projection hydrated from the execution on reload and shown under
`checkpoint.subject` in status; it is never stored on its own. Graphs already in
flight at upgrade load with `subject` absent and status omits the field.

## SQLite stores

### A format major permits additive evolution and one-shot migrations

Each SQLite store names its `user_version` base. Releases normally keep the
base fixed within its state-root format major, so binaries of the same major
read and write it whatever their release minor. Each owner declares one ordered
list of additive steps tagged with the
release minor that added them: tables and indexes with `IF NOT EXISTS`, and
columns that are nullable or have a non-NULL constant default. Steps never
rewrite, drop or rename, and every insert names its columns, so an older writer
ignores newer columns. A writable open creates the base and any missing steps
atomically, re-checking under the same immediate transaction; a current store
opens without a write. A reader ignores unknown objects but refuses a missing
known base table or declared step, and a different root major is refused without
changing the store.

A change to stored values that a rename or a removed type makes unavoidable is
declared once as a migration from the old base to the new one. A writable open
runs every declared migration in order inside the same immediate transaction,
then records the new base; a second open does not run it again. A read-only open
of an older base refuses and names the writable open that migrates it. A binary
that predates the new base refuses the store, so the migration is the one
crossing and there is no compatibility path afterwards. Owners keep the SQL
narrow: rewrite only the rows and fields the new types cannot read, and make
every walk total for any valid JSON. A row whose stored bytes are not valid JSON
is skipped by the rewrite and fails only its own decode, because one damaged row
must not block every writable open.
- Why: additive changes preserve stored state, while a single declared rewrite
  lets a vocabulary change reach existing rows without a permanent decoder
  shim.
- Owner: `crates/sqlite/src/schema.rs`, each store's schema declaration

## Records

### Commit before signal

Owners commit durable state before the lossy change-generation signal; a failed
signal never fails the committed write. Readers treat an absent or reset
generation as unknown and re-read.
- Why: a hint must not become a second source of truth.
- Owner: `crates/os/src/changes.rs`

### Strict, bounded records

Unsupported versions fail before mutation. The reader rejects ambiguous or
unbounded input and never echoes the rejected value. Closed codecs reject or
discard unknown fields; records with explicit extension maps preserve only
unknown fields, so a newer writer's data survives a round trip. Malformed values
fail decoding instead of becoming absent facts, and read failures name the path
and record type (with the schema version when readable) and the missing field,
never the rejected value. Additive optional fields default to absence.
Edge-built facts are `Serialize` only.
- Why: a forged file must not become evidence.
- Lint: `scripts/check-extra-fields` rejects host literal and constant keys in
  non-test extension-map reads and writes, with no allowlist.
- Owner: `crates/records/src/`

### One digest preimage

`CanonicalV1` bytes are deterministic and domain-framed; it is not RFC 8785.
- Why: a digest is an identity only if every writer computes the same bytes.
- Owner: `crates/records/src/canonical.rs`

## Configuration

### Config never widens authority

Syntax, version and filesystem-boundary failures refuse loading; an invalid
optional value warns and uses the default, and a malformed restriction never
becomes permission. Commands load only their declared domains; invalid unused
domains warn. `config set` uses the same tolerant read as `config check` and the
same typed schema as `config apply`, warns on unknown keys, keeps them and
publishes atomically. `config show` reports each value with its source.
Redaction is owned by [supervisor](supervisor.md#disclosure-and-safe-git-evidence).

Project binding directories must be owned by the current user and private from
group and other users. A permission or ownership failure carries a local repair
diagnostic; malformed bindings and filesystem errors keep their own causes, and
lookup never repairs state. First publication, hint replacement and removal share
a binding lock; contended readers, including GC, retry under its shared side.
A binding record can be forgotten when its common
directory is gone: the smoke verifier cleans up its scratch binding at exit, and
GC retires a vanished binding once its record has been idle for `gc.idle`. The
private host-project directory stays for a later bind. Peer status and events
replace local paths in a terminal cause with a safe reason and remedy, taken
from the event's own recorded cause, and disclose remote paths only through an
approved anchor.
- Why: errors need visible remedies without expanding authority.
- Owner: `crates/config/src/`

### Every setting reports its layer

Every setting value travels with the layer it came from and, when it has one,
the exact origin. One vocabulary names the layers for every family, so a warning
or receipt never invents a second spelling. A family declares its precedence
once, lowest first; the merge takes the candidate from the highest listed layer,
keeps the last given on a tie, and returns every other listed candidate as
shadowed. A candidate from a layer the family does not list is ignored and
reported, so the caller can warn with its source. The merge and every
explanation read that one declaration, and a limit receipt keeps its bytes
because the overlapping layer names are the same spelling.
- Why: one vocabulary lets every explanation name a source the next reader can
  act on, and a family cannot rank the same setting two ways.
- Owner: `crates/records/src/provenance.rs`, each family's precedence constant

The full set of families, each declaring its precedence once, lowest first,
beside the type it owns, is:

- Limits use `LIMIT_PRECEDENCE`: default, host, project, tier, template, launch.
  The shared layer spellings match the old `LimitSource` bytes, so digest-sealed
  run requests keep their exact wire form and need no rewrite.
- Ports use `PORT_PRECEDENCE`: default (formerly `builtin`), detected, host,
  host-project, project. Stored receipts and port tables rewrite `builtin` to
  `default` once at install.
- Pressure thresholds use `PRESSURE_PRECEDENCE`: default (formerly
  `shipped_default`), host (formerly `configuration`).
- Admission classes use `ADMISSION_PRECEDENCE`: default, host, host-project,
  project. This is distinct from pressure thresholds; admission class config
  resolves with its own declaration, not `PRESSURE_PRECEDENCE`.
- Lane access uses `LANE_ACCESS_PRECEDENCE`: default, host, project, tier,
  template, route (formerly `route_policy`), launch. The resolved route policy
  is a `route` candidate and outranks the template or tier inputs that selected
  it.
- Sandbox mode uses `SANDBOX_PRECEDENCE`: default, host, route, parent. A knob
  records its actual layer; a platform or restriction is a `constraint` beside
  the layer, not a source of the value.
- Knobs use `KNOB_PRECEDENCE`: default, profile, host. A launch flag is recorded
  separately in the sealed profile and is not a configuration layer.
- Config overlays use `OVERLAY_PRECEDENCE`: default, host, project. Host-project
  state reports as the host layer to owners.
- Editable schema fields use `SCHEMA_PRECEDENCE`: default, profile, host.
- Land policies use `LAND_PRECEDENCE`: default, host, project.
- Graph templates use `TEMPLATE_PRECEDENCE`: default, host, project, launch.
- Pool weights use `WEIGHT_PRECEDENCE`: default, host, project.
- Observation retry uses `RETRY_PRECEDENCE`: default, profile, host, project,
  launch. The `ci.retry.*` values are one family shared by every CI reader.
- Admin publication grants use `ADMIN_PUBLICATION_PRECEDENCE`: default, host. A
  grant from another layer is ignored and warned with its source.

Every production resolver builds its candidates and merges them with its own
const; the const is the single declaration of order, and a candidate from a
layer the family does not list is ignored and warned with its source.

The run store's base-13-to-14 migration (`crates/run/src/migration_13_14.sql`)
is the one rewrite for these renames. On a writable open it rewrites each stored
attempt record: `route_policy` becomes `route`, a sandbox `restriction` or
`platform` becomes a `constraint` beside the `host` layer and a `knob` becomes
`host`, a resource fact's `configuration` becomes `host` and `shipped_default`
becomes `default`, and a port binding's `builtin` becomes `default`. The
`lane_ports` and `pacing_reserve` facts are exceptions: their origins are a
`PortSettingSource` (`configuration`, `shipped_default`, `recorded_plan`) and a
`PacingSettingSource` (`configuration`, `shipped_default`, `unresolved`), not
`Layer`s, so those names stay byte-identical. The same migration moves v2.8.2
flat usage buckets (`uncached_input`,
`output_excluding_reasoning`, `cache_read`, `cache_write`, `cache_write_1h`,
`reasoning`, including a retained session total) under `tokens` and maps the old
free-form source to the typed `stream` source, preserving every stated number.
A malformed row is skipped so it fails only its own decode; a row already in the
new shape is left byte-identical. No decoder accepts a retired spelling.

### One sourced port table

One table declares forge operations, verify, land, CI observation, worktree
layout, naming, post-create, tracker, release, attempt admission and the build
interface. Layers apply `builtin` < `detected` < `host` < `host-project` <
`project` ([layout](../runbooks/configuration.md#port-table)). Each layer deep-merges
object fields over the lower one; provenance is tracked per dotted leaf and a
port's source derives from its leaves, so a serialised source never overrides it.
A host-project layer configures a consumer repository with no files in its tree.
A missing layer is skipped silently; a malformed one names its file and key, a
bad project overlay warns and is skipped so commands that do not need verify
still run, and an unreadable host table still refuses lane identity. `verify.base`
is inferred from the remote's default branch when no layer sets it, and
`config show` names the source.

Forge operations, CI observation and tracker share one typed port and one
Builtin/Command/None adapter in `config::ports`; `none` disables a port without
selecting the builtin. A command port runs its configured argv directly with no
shell, inherits only the declared execution-context environment plus
owner-supplied values and never forwards ambient credential or agent variables.
Declared exit codes select succeeded, unknown or refused, and incomplete capture
is unavailable. The land owner resolves `PortTable.land` before publication
effects and seals it in the landing plan. Command receipts under
`port-receipts/<owner>/` are bounded and omit argv, environment and output; a
visible publication with unconfirmed durability stays a typed visible receipt.

Admission ports give the repository its own semaphore keyed by attempt and epoch:
acquire and release are idempotent, release fences a late acquire for the same
epoch, and a queued retry uses a new epoch. Cancellation retains admission until
preparation unwinds, the command and source are frozen privately before
acquisition, and release runs outside terminal transaction locks. A matching
successful receipt proves release after terminal settlement without changing the
immutable attempt record, and busy acquisition returns the attempt for retry.
Environment variables for port commands are in the
[consumer contract](consumer-contract.md#environment).
- Why: one declared layering lets a repository be configured from host state or
  its own tree with one provenance rule.
- Owner: `crates/config/src/ports.rs`

### Build state has one host root

The build store, queue, ledger and maintenance record share `<state>/build/`. The
ledger is host-level so the soak spans lanes, the chair and landing and survives
attempt GC; when it is unwritable the row goes to the attempt scratch and the
report counts it missing. Build slots live in the host cache directory; a slot's
`repo/` is a clone of the store with hardlinked objects, never shared
alternates, and `slot.json` lives inside the slot so an OS cache purge removes
both. A slot stays occupied until the holder and every build child it spawned
are gone.
- Why: one root to report, GC and document, and a slot record must never
  disagree with its slot.
- Owner: `crates/run/src/build/paths.rs`, `crates/run/src/build/ledger.rs`

### Retention keeps evidence

GC retains evidence and unimported Git work, expires resumable provider state
only after archiving history, skips busy items, protects active reservations and
continuations, and deletes by identity without following links. Usage retention
belongs to [telemetry](telemetry.md#the-usage-ledger-outlives-the-run-store);
scratch and cache eligibility, sweep budgets and resumption to the
[disk spec](disk.md); worktree and branch removal to
[worktrees](supervisor.md#pruning-and-gc-need-proof).

`RETENTION_CLASSES` is the executable declaration of every retention class (typed
owner and adapter, triggers, eligibility, schema settings); scheduled tick and
explicit GC run the same registry path, each class through its own adapter, with
no wildcard adapter. Byte rows come from adapter measurements marked complete,
partial or unavailable, and collection failures stay visible. Admission command
receipts become eligible only once their attempt is terminal and release is
proven. GC resumes interrupted checkpoint collection under the graph lock while
the name reservation survives. Removal records keep subject, tip, subsumed or
landed predicate and evidence source; shallow history, conflicted merge trees,
an offline forge and rewritten default branches stay typed kept outcomes.

Scheduled collection runs daily from `provenant tick`, and the daily deadline
advances only when a collection completes: launching a detached worker moves
nothing. A worker that fails, cannot launch or is interrupted leaves the day due
and is retried with bounded backoff, counted in `failures` in `gc-result.json`
and reported once per tick as `gc=retry_pending`. A collection that stops at its
sweep budget with a saved cursor ends `partial`: it is progress, resets the
count and is resumed by the next sweep. An unreadable result (corrupt, over its
cap, from a newer writer) is treated as absent, reported once and replaced; a
result dated in the future is stale. `gc-result.json` is rewritten whole, so a
writer does not round-trip fields it does not know.
- Why: freeing space must not destroy evidence or a session in use, and one bad
  result must not block collection.
- Owner: `crates/config/src/gc.rs`; `crates/confinement/src/worktrees.rs`;
  `crates/run/src/retention.rs`

## Release identity

### The source digest is the binary's identity

It covers shipped source, lockfile, toolchain, packager and bundled assets, with
no override; test-only edits do not change it.
- Why: the installed binary must be provably the reviewed source.
- Owner: `build.rs`, `crates/records/src/source.rs`

### Releases are immutable

Install verifies and smoke-tests a release before trusting it. `release.json` is
published last and a published release is never repaired. A digest proves
identity, not publisher trust.
- Why: a partial or swapped release must never run.
- Owner: `crates/config/src/release.rs`

## Managed install

### Harness integrations have one declaration

Managed harness paths and native hook event mappings come from `AdapterSpec`, and
every supported instruction target links the same release instruction asset. An
unavailable integration names its reason and source in install findings.
Provenant has no MCP integration: the CLI is the agent interface.
- Why: adapters share capability intent while native formats stay replaceable.
- Owner: `crates/records/src/adapters.rs`, `crates/config/src/harness.rs`

### Preview before every write

A failed preview prevents writes, and `--dry-run` creates no root, lock or
receipt. Identity and bytes are rechecked before each publication; drift stops
the install.
- Why: the user's harness config changes only in view.
- Owner: `crates/config/src/managed.rs`

### Only proven-owned entries change

Removal unlinks only entries whose value still matches the receipt, never
recursively; adoption does not authorise deletion. After a home or state-root
move, stale entries are reported without their paths being inspected.
- Why: an old path may now belong to something else.
- Owner: `crates/config/src/managed.rs`

### Foreign bytes and user hooks survive

Edits replace only owned values, keeping comments and order; malformed or
symlinked configs are refused unchanged. Edited hooks are not overwritten. A
missing container is created only as an object, so an index selector reaches an
array the document already holds. An upgrade that retires an owned hook action
hands its ownership to the declared replacement, so installing without
`--hooks on` keeps the managed set.
- Why: a harness config holds the user's other tools and secrets.
- Owner: `crates/config/src/splice.rs`, `crates/config/src/hooks.rs`

### LaunchAgents are owned like any entry

The peer-agent and tick plists join the preview and receipt. A symlink at the
plist or a parent is refused, and an existing plist is adopted only on an exact
byte match.
- Why: a LaunchAgent runs unattended with the user's authority.
- Owner: `crates/config/src/managed.rs`, `src/tick_agent.rs`

### Re-apply is the recovery

A partial install lists applied and remaining actions, and rerunning `install`
recovers it, even with no receipt. There are no rollback journals.
- Why: one idempotent path is easier to trust than a rollback engine.
- Owner: `crates/config/src/managed.rs`

### Format changes migrate once at install

An install-time migration step owns a change to non-SQLite state: files,
ledgers, receipts and metadata that no SQLite store owns. SQLite stores migrate
themselves on writable open and are not wrapped here. Steps run in declaration order after
the live-attempt advisory and before any release is swapped in, each only when
the install receipt does not already record its id. An id is recorded only
after that step succeeds; a failed step warns `MIGRATION_STEP_FAILED` with the
state path, leaves the receipt unchanged, and the install proceeds. Every step
is idempotent, so a rerun after a partial failure finishes the job, and a step
is deleted in the release after the one that ships it. A first install has no
state to migrate; the receipt it publishes records any later step's id.
- Why: state a store does not own still changes shape, and one recorded
  crossing keeps retries safe without a permanent decoder.
- Owner: `crates/records/src/migrate.rs`, `src/install/migrations.rs`

`ReadSourceLiveSpelling` (`read-source-live`) rewrites the retired
`lanes.read_source` value `checkout` to `live` in the host and host-project
config files. It is idempotent, reports each changed path, and never rewrites a
repository's own `.provenant/config.toml`; a repository file still saying
`checkout` warns `READ_SOURCE_VALUE_RENAMED` and resolves as `auto`. No other
batch-P rename needs a persisted step: the other names are unreleased or in
memory only.

## Typed host metadata in format 2

Every host fact has one typed field on its owning record, shared by writer and
reader, and never lives in an extension map: `Trace.telemetry_context`, the
attempt's `repository_admission_binding`, sandbox mode and admission receipts,
preparation failures and guard evidence, graph specification, attempt and route
metadata (provisioning, placement, review, peer, template), the peer planner
fallback reason, the wait observation report and telemetry's work-context PR,
lens, task and risk. Optional facts may be absent, which reads as unknown; a
present value of the wrong JSON type fails decoding. Every new persisted field is
optional or defaulted, and flattened extension maps keep unknown fields from
newer minor writers without interpreting them as host facts.
- Why: each host fact gets one shared writer and reader type, so no string key
  decides authority.
- Owner: `crates/records/src/trace.rs`; `crates/records/src/run.rs`;
  `crates/fabric-runtime/src/graph/model.rs`;
  `crates/fabric-runtime/src/coordination/facade.rs`; `crates/fabric-runtime/src/runs.rs`
