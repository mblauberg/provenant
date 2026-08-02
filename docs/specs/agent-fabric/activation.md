# Agent Fabric activation and operations

[Issue #23](https://github.com/mblauberg/provenant/issues/23) records the
completed delivery that established these requirements. A change to them
requires a current linked GitHub issue and Project Status.

The current contract closes effective-configuration identity, subject lineage and permission semantics across activation, smoke and provider actions. It permits authorised write-capable generic work while retaining enforced read-only as a hard certifying-review requirement. Every active adapter publishes the shared capability snapshot and effective launch configuration, with requested, actual or honestly unknown route identity in its activation evidence.

## Outcome

Promote the coordination-only agent fabric into a safely activated local model-execution fabric for Claude, Codex, Agy, Cursor and Kiro, with Pi ready but unavailable until an open-weight provider/model is installed. Add operator-started human-readable Herdr observation and coordinated seat rotation without weakening authority, disclosure, certifying-review/Kiro read-only boundaries or fail-closed compatibility gates.

## Required behaviour

1. Every activated adapter is bound to Git wrapper provenance: the wrapper entrypoint must be tracked at the owning repository's HEAD and recorded as repository commit plus wrapper path in the composed adapter evidence. Immediately before every adapter process spawn, the wrapper is re-derived, its bytes are checked against HEAD and its repository commit and path are checked against the composition pin, so an untracked or locally modified wrapper fails closed at spawn. Provider CLIs are admitted through their stable launcher, adapter-specific vendor identity and bounded non-answer interface; observed version and digest never gate a normal update. Protocol schemas and Fabric SDK libraries remain hash/lockfile verified. The tsx loader that executes tracked TypeScript source is a lockfile-pinned third-party dependency.
2. Provider work uses the admitted absolute working directory and exact matched
   permission profile. Generic work may use write tools/edit modes only when its
   task authority and matched profile explicitly grant them; approval bypasses,
   extra roots and uncontrolled provider/model substitutions remain forbidden.
   Certifying review always requires an enforced `read-only` profile.
3. Malformed, drifted or ambiguous provider responses fail closed before state is accepted.
4. Kiro uses a real ACP v1 client with bounded framing, capability negotiation,
   session lifecycle and read-only tool policy; its signed stable launcher and
   initialize handshake are revalidated without a CLI version lock.
5. Activation is staged and reversible. One adapter failure cannot disable coordination or corrupt another adapter's journal.
6. Provider-backed smoke tests use bounded read-only prompts, record the
   admitted adapter, canonical executable and observed identity plus the
   explicitly requested model route, reject wrapper-visible substitutions, and
   may consume quota under this approval. Upstreams that do not report an
   effective model must not be described as independently proving it.
7. Herdr observation reads a durable monotonic event cursor and renders one-line summaries in a separate local observer pane. Message events include a terminal-safe 160-character body preview. It never types into an agent composer, receives mail or acknowledges delivery.
8. Seat expiry warnings are automatic. Authority extension remains an explicit operator action: close the old run only after daemon-produced barrier evidence, provision a fresh immutable generation, atomically cut over the roster, reconnect every seat, and run health plus round-trip smokes. The global 31-day maximum remains non-configurable by projects.

Executable resolution is revisioned at `2`: configured command names are
resolved natively against the current `PATH`, and the resolved file must be a
regular executable. A missing or unavailable enabled optional adapter is
reported as degraded and omitted from daemon composition, status and doctor;
an unavailable mandatory primary remains fatal. A directly selected optional
adapter still fails closed when its executable is unavailable. This executable
check is separate from activation: `direct-cli` does not turn disabled Pi into
Fabric activation, while the Fabric gate continues to reject disabled Pi.

## Fresh coordination-run launch

A trusted local Console starts the first current-schema coordination run through
the same project-session lifecycle used thereafter. It creates the session,
selects the session-bound operator capability, and moves the reviewed launch
packet to `awaiting_launch`. The negotiated `launch-custody.v1` extension then
exposes:

```text
fabric.v1.project-session-launch.prepare
```

Its request contains only the operator command context, `projectId`,
`projectSessionId`, `expectedSessionGeneration`, and the reviewed
`launchPacketRef`. The daemon reads the current project/session rows, closes the
adapter contract and resource plan, normalises the chair authority, computes
all CAS and digest bindings, validates the resulting launch intent, and returns
an ordinary consequential `OperatorActionPreview`. The Console commits that
preview with `fabric.v1.operator-action.commit`; it never authors the launch
intent or its custody digests.

Preparation is read-only and command replay returns the same preview. An
interruption before preview persistence is retried with the same request. A
commit interruption uses operator-action status plus idempotent commit replay and
launch-specific recovery;
a proved no-effect launch moves to `launch_failed`, from which preparation
derives the exact failed action and accepts only a fresh packet, run and
provider-action identity. Ambiguous provider effect remains quarantined and is
not replayed.

After the chair launch commits, the chair creates or attaches the Claude peer
through the existing agent protocol. Only then may the existing `mcp provision`
command bind a complete Codex/Claude roster and install an immutable seat
generation. `mcp provision` remains renewal-only: it still requires an active
run, current chair lease and at least two existing agents. The unprovisioned MCP
server remains available with no Fabric tools or resources until that sequence
has completed. This path adds no provider login, provider activation, database
migration or new authority model.

## Activation order

1. Claude Agent SDK.
2. Codex app server.
3. Cursor and Agy headless boundaries.
4. Pi RPC isolation and runtime capability conformance; activation waits for an available trusted open-weight route.
5. Kiro ACP.
6. Herdr observer.
7. Coordinated seat renewal.

Each step must pass compatibility, boundary, conformance and negative tests before joining `activeAdapters`. Provider-backed smoke follows activation and stops on any write attempt, schema drift, unexpected permission request, missing session reference or unbounded output.

The current activated optional-reviewer identities are Agy
`Gemini 3.1 Pro (High)` (`google`, high) and Cursor
`cursor-grok-4.5-high` (`xai`, high). They use the existing provider
subscription sessions. Adapter subprocesses receive no ambient provider API
key, and no provider credential is stored in Fabric. Activation remains exact:
the current executable identity, wrapper provenance, runtime handshake,
negotiated protocol, capability checks, explicit model, family and
model-encoded effort must all satisfy the checked contract or new work fails
closed. Installed provider version and executable, bundle or schema digests are
not compatibility gates.

## Non-goals

- No provider credential export or login changes.
- No automatic public deployment or Git push.
- No unbounded fabric message bodies in Herdr; local previews are capped and terminal-neutralised.
- No authority extension by capability rotation or blind timer.
- No fallback that bypasses a disabled, inactive or runtime-nonconformant adapter.

## Rollback

Restore `activeAdapters: []`, restart the visible daemon, retain journals and seat generations for audit, and rerun coordination-only health plus Codex↔Claude mailbox smokes. Adapter activation is configuration-reversible. The current squashed database baseline includes the observer event-sequence table; rollback retains its monotonic audit rows because removing them would destroy cursor history. No numbered predecessor migration or compatibility path is retained.

## Acceptance

- Full runtime and harness gates pass.
- Every adapter has positive conformance and negative boundary coverage.
- Provider-backed read-only smoke passes for each available logged-in provider/model family; unavailable account models are recorded, not substituted silently.
- Herdr observer resumes without loss after an orderly restart, provides at-least-once rendering across a crash window, shows bounded local message previews and exposes no capability data.
- Expiry warning and explicit coordinated rotation tests pass.
- Multiple targeted lenses and an independent other-primary review report no unresolved P0–P2 findings; terminal pressure follows `HARNESS.md`.

## Capability and effective-route evidence

Activation now requires the exact shared `adapterCapabilitySnapshotV1`,
`deployedRouteAdmissionV1` and `deployedRouteObservationV1` codecs owned by
the [provider actions and adapters contract](provider-actions-and-adapters.md). This specification
adds no competing schema.

An adapter may enter `activeAdapters` only when its current `kind: available`
capability snapshot
binds the activated executable/package, wrapper provenance, adapter contract,
host/version, model catalogue, raw effort values, raw native-mode values,
context boundary claims, orchestration bounds and enforceable permission
source. The
snapshot source is exactly `runtime-discovery` or `capability-fixture`. A
capability fixture cannot be reported as runtime discovery and cannot admit an
adapter whose current bounded non-answer interface fails. A
`source/kind: unavailable` snapshot is persisted negative
evidence but cannot activate the adapter or admit answer-bearing work. Expiry
or contract drift removes the adapter from new automatic admission without
rewriting prior receipts.

`safety.enforcedReadOnly` is a capability fact, not a global permission mode.
`true` is mandatory before the adapter/profile pair can advertise certifying
review. `false` may activate generic answer-bearing work only when the exact
permission profile, task authority and launch envelope admit the requested
writes. `unknown` cannot certify review and cannot satisfy any task that depends
on enforced read-only. No route gains write authority from activation alone.

Every activation, provider-backed smoke and answer-bearing provider action
stores one closed
`adapterEffectiveConfigurationV1` beside the shared snapshot and route lineage:

```yaml
adapterEffectiveConfigurationV1:
  schemaVersion: 1
  configurationId: stable-id
  configurationRevision: positive-contiguous-integer
  adapterId: exact-adapter-id
  adapterContractDigest: sha256-prefixed-digest
  executableIdentityDigest: sha256-prefixed-digest
  capabilitySnapshotRef: capabilitySnapshotRefV1
  subjectKind: activation | provider-smoke | provider-action
  subjectRef:
    oneOf:
      - activationId: exact-activation-id
        activationRevision: positive-integer
      - smokeId: exact-smoke-id
        actionRef: ProviderActionRefV1
      - actionRef: ProviderActionRefV1
  subjectRefDigest: sha256-prefixed-digest
  activationConfigurationRef:
    oneOf:
      - null
      - configurationId: exact-activation-configuration-id
        configurationRevision: exact-activation-configuration-revision
        configurationDigest: sha256-prefixed-digest
  requestedConfigurationDigest: sha256-prefixed-digest
  effectiveConfigurationDigest: sha256-prefixed-digest
  permissionProfileDigest: sha256-prefixed-digest
  discoverySurfaceRef: discoverySurfaceRefV1
  ignoredOrUnsupportedFields: [exact-field-paths]
  permissionSource: adapter | host | config-overlay | unknown
  observedAt: timestamp
  configurationDigest: sha256-prefixed-digest
```

The object and each subject arm are closed; field paths are sorted and unique.
Subject kind selects exactly one matching ref arm. Activation requires null
`activationConfigurationRef`; smoke/action require the exact current activation
configuration for the same adapter/contract/executable and cannot cite another
subject. `subjectRefDigest` is SHA-256 of RFC 8785 JCS of the selected closed
subject-ref arm. `subjectKind` plus that exact selected ref is the sole subject
identity; there is no caller-authored parallel ID. Per adapter, one activation
ID/revision or smoke ID owns one effective configuration, and one canonical
provider action pair owns one effective configuration. The database enforces
those discriminator-specific identities independently of the digest.
`(configurationId,configurationRevision)` is immutable and unique.
`configurationDigest` is SHA-256 of RFC 8785 JCS over the complete object with
only that field omitted. Capability instance/body, requested/effective,
permission and discovery-surface identities equality-bind the shared route and
launch evidence.
Host-global settings remain user-owned. Fabric generates only a minimal
per-run overlay inside existing authority, records every unsupported field and
does not silently persist global defaults or hooks. Smoke/action rows record
their effective view and never update either the activation row or global host
configuration. the operational-hardening contract owns the generated schema, immutable persistence,
registered evidence and cross-row constraints; this specification owns the
activation/evidence semantics.

Smoke evidence round-trips the exact requested identity and the shared admitted
identity. Where the provider reports actual host/adapter/provider/family/model/
effort/native-mode values, they populate the observed route arm with its exact
source and confidence. Where it does not, those observed fields remain null
with `source: unavailable` and `confidence: unknown`; the admitted value is not
copied into actual. An adapter whose required actual field is unknown is
ineligible for a gate requiring that attestation.

Conformance adds positive and negative fixtures for snapshot expiry, binary or
contract drift, raw-effort/native-mode round-trip, ignored configuration,
provider substitution, subject-arm/activation-lineage crossing, permission-
profile mismatch, duplicate activation/smoke/action subject refs under different
configuration IDs/digests, honest unknown actual identity and point-of-use body-stable
capability revalidation. Subscription/login changes, OpenCode activation,
paid-region selection and global model/effort preference changes remain
separate human gates.
