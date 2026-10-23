# Routing port status

Rulebook: PORTING.md v4 / ADR 0028. Inert library; no command cut-over.
Author and native review route: `codex/gpt-6.1-sol@high` (the supplied Fabric route).
Source/oracle baseline: `49a1cc3bceedcd0fb4c14183f6afff3ea01f6a84`.
Owner integration baseline: `114e3331ebf6b767baadac1069d4b900ae7d91d2`.
Main was merged, never rebased. Python differentials use `oracle.rs`, `-I`, an
explicit isolated environment, file-backed streams and bounded observation.
The local interpreter is CPython 3.14.8. ISO offset fixtures probe interpreter
behavior and choose core's explicit 3.14.4/3.14.8 compatibility mode.

## Fully ported and retained

- Retained from the preceding batch: `scripts/model_route_live.py#unit-02`
  (fallback/OpenCode discovery decisions) and
  `scripts/model_route_pools.py#unit-03` (human listing rendering). Their tests
  remain in the required routing suite. These IDs were already in ported.txt.

## Continued units and exact blockers

No additional full-unit claim is made in this continuation. Every assigned unit
has either retained full coverage or a coherent implementation slice below;
codec and composing-owner gaps prevent broader acceptance.

- `scripts/model_route_catalog.py#unit-01`: validators retained; shared core
  compatibility owners adopted. RP-130402's expanding set-based family alias
  policy remains unresolved. No new acceptance is inferred.
- `scripts/model_route_cli.py#unit-01`: defaults and command dispatch prefix now
  cover legacy picks, selection, snapshots, health, listings, cooldowns, reset
  and picks, with exact output/exit differential fixtures. Still partial for
  argparse edge behavior (including unique long-option abbreviations), JSON
  extended values and the resolver continuation.
- `scripts/model_route_cli.py#unit-02`: the probe branch now passes explicit
  listing-only deadlines and preserves output/exit codes. The resolve and
  classification tail needs the unfinished model_route resolver; it remains an
  explicit continuation rather than a fake successful command.
- `scripts/model_route_pools.py#unit-01`: ordered merge/migration/reject policy,
  numeric equality, source hashability failures, privacy/model resolution and
  installed-binary lookup are implemented. Full Python object ingress awaits
  records' non-finite/lone-surrogate JSON value support.
- `scripts/model_route_pools.py#unit-02`: availability, compatibility filtering,
  effort, council, rotation, pick/pick_many and describe policies are present.
  A raw state transaction owns source schema reset/project/route/cursor policy.
  Still needs a concrete safe lock/publication adapter in an allowed composing
  crate, plus config's adapter-compatibility document loader. Routing cannot
  depend on supervisor under R2. No second filesystem or YAML parser is added.
- `scripts/model_route_preferences.py#unit-01`: ordered candidate ingress,
  source-order hard admission, typed refusals, status repr, preferences and CLI
  default paths are implemented. Full claim is blocked on records' Python
  extended JSON ingress (loaded-versus-corrupt differs for NaN/surrogates).
- `scripts/model_route_preferences.py#unit-02`: receipts, exact spreading-state
  validation, fair selection and publication-before-success are implemented
  through one locked transaction. Concurrency and publication failure tests
  survive. Extended JSON ingress and concrete safe filesystem composition remain.
- `scripts/model_route_live.py#unit-01`: ordered suffix precedence, model
  matching, advisory reader/quota seams, cooldowns and recent failures are
  implemented. Core timestamps retain Python's complete datetime year range;
  records owns cooldown reads. Extended JSON advisory ingress and safe reader
  composition remain, so this unit is still partial.
- `scripts/model_route_probe.py#unit-01`: listing parsers, JSON duplicate
  refusal, state-root policy and Kiro read-only request/event/metadata policy
  are implemented. The all-adapter probe/cache state machine remains a coherent
  follow-up needing permitted bounded process/env and safe scratch/cache APIs.
- `scripts/model_route_probe.py#unit-02`: provenance/age/effort/identity/order
  validation retained; timestamps now use core. Still inherits RP-130402 and
  records' extended JSON value gap.
- `skills/orchestrate/scripts/capabilities.py#unit-01`: table-driven Codex,
  AGY and Claude discovery, normalizers, auth/canary decisions, bounded process
  requests and scrubbed failures are implemented. Missing records APIs for
  Python extended JSON and JSONDecodeError wording keep it partial.
- `skills/orchestrate/scripts/capabilities.py#unit-02`: argument policy,
  dispatch, warnings, output permission requests and exits are implemented.
  It remains partial for the discovery JSON error/extended-value gaps.
- `skills/orchestrate/scripts/exec_routing.py#unit-01`: route/snapshot requests,
  registered identities and exact malformed-catalogue warning classes, candidate
  expansion, training/confidentiality and account reselection are implemented.
  Snapshot JSON extended values still require the records owner.

- `skills/orchestrate/scripts/exec_routing.py#unit-02`: scoped records reads,
  advisory quota reconciliation, explicit pin behavior and exact Python datetime
  maximum selection are implemented and differentially tested. Canonical model
  lookup still inherits unit 01's extended JSON snapshot gap, so cooldown results
  can differ on those inputs. This unit also remains partial.

## Shared owners and pending APIs

Core owns strip/truthiness/equality/str/repr/decimal digits/splitlines and ISO
parsing. Records owns checked JSON grammar/range and scoped cooldown records.
Their replaced routing copies were deleted. The remaining private copies are
marked with their owner:

- `pending/casefold.rs`: core has no Unicode casefold API yet.
- `pending/ordered_json.rs`: records has no public ordered generic value codec.
- `pending.rs`: a checked generic ordered decoder bridge, ordered container repr,
  and Python-compatible ordered JSON text encoding. Records' Value-only codec
  cannot supply these order/byte contracts; core primitive renderers are reused.
- Quota route/account advice has explicit traits; those operations are absent
  from quota's public display/schedule API. No quota implementation is copied.
- `CompatibilityDocumentLoader` is the single YAML/current or JSON/future seam.
  Config has no public adapter-compatibility loader on the integration baseline.
- Selection/rotation/process/scratch interfaces request effects from an allowed
  composing owner. They do not use unsafe, own a child, or duplicate supervisor.

No intentional behavior change beyond the already-authorized exact weight and
R4 integer-range improvements. Remaining mismatches are recorded as open in
`divergences.md`; partial IDs are not appended to `ported.txt`.

## Verification and review

- `cargo fmt --all`: passed.
- `cargo clippy -p provenant-routing --all-targets -- -D warnings`: passed.
- `PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-routing`: 128 passed,
  zero failures, zero ignored; zero doc tests.
- Two direct layering-policy checks passed (pytest is not installed).
Independent native policy/runtime source reviews found concrete failure-path
and wire differences; repaired cases have surviving differential fixtures.
J1/J2/J3, platform conformance, CI and chair-owned cross-family review remain
landing/cut-over gates. Auto-merge is not armed.
