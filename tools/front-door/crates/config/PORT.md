# Config port batch 3

Rulebook: `docs/specs/rust-port/PORTING.md` version 4. Author route:
`codex/gpt-6.1-sol@high`. Initial source/oracle revision:
`7021182d535f6c730c29713e77d5bca76893fd06`; merged fetched main
`b527d675d15c50ee51701bb15db6439c95277918` without rebasing. The assigned Python
sources are unchanged between those revisions. Python oracle: 3.14.8; Cargo:
1.94.1, under the workspace toolchain pin. Tests/judges in this PR are the crate
unit and differential fixtures; independently owned J1/J2 coverage gates cutover.

No command switches owner. No Python/TypeScript source or stable installation is
changed. Runtime roots, clocks, profile environments and execution capabilities
are explicit inputs; fixture mutations stay under isolated temporary roots.

## Unit ledger

| Unit | Status | Completed scope and remaining boundary |
| --- | --- | --- |
| `scripts/configure-codex-skill-overrides.py#unit-01` | Partial | State/proposal/preflight/configure/check, symlink protection, atomic publication, newline conversion, Python whitespace, collection-error precedence, typed command options. Exact tomllib diagnostics and the uncaught TypeError/Unicode traceback boundary remain (RP-config-3/4). |
| `scripts/install-provenant-command.py#unit-01` | Partial | Classification, exclusive stage/hardlink installation, atomic exchange, guarded rollback, displaced-file recovery retention, fsync/chmod, expected-state validation and command rendering. Platform/race conformance, non-UTF-8 diagnostics and syscall-specific error forms remain; ordinary missing-source errno output is differential-tested. |
| `scripts/install-workflows#unit-01` | Partial | All install/repair/retire/preflight/integrity and command policy branches represented, shared publication owner adopted, ordered validation and strict entry keys fixed. Extended JSON/path domain and exact decode diagnostics remain (RP-config-2/4/6). |
| `scripts/instance_installation.py#interface` | Ported contract | Typed action/request, desired-state, pointer, seed, routing and untagged execute result contracts, with an `InstanceOwner` cycle seam. Every real execute result shape round-trips against Python. This claims the interface only; instance seeding/routing implementations and extended path encoding remain outside it. |
| `scripts/lib/front_door_build.py#unit-01` | Partial | Digest, embedded identity, prebuilt validation, build plans, build/smoke/select policy and command rendering. `FrontDoorHost` owns execution; `ExecutableAccess` requires X_OK for PATH and fallback, with no mode-bit approximation. A safe supervisor `access(X_OK)` adapter and an execution adapter preserving cargo progress/capture/deadline policy are not on main yet. |
| `scripts/managed_installation_manifest.py#unit-01` | Partial | Validation, tree digests, opaque history, exact integer decoding, insertion-ordered ownership, ordered first-error diagnostics and sorted atomic persistence. Nonfinite/lone-surrogate JSON, Unix byte paths and exact decode diagnostics remain. |
| `scripts/manage_installation.py#unit-01` | Partial | Exact skill-marker glob discovery, source/library/custom collision policy, link identity, legacy agent validation/retirement, staged directory conversion with restore-on-publication-failure and typed reports. Legacy receipt/path codec edge cases and platform publication conformance remain. |
| `scripts/manage_installation.py#unit-02` | Partial | Managed/custom plan transitions, integrity, remediation/refusals, read-only source/legacy reports, all early execute branches. Extra/legacy names with non-UTF-8 bytes and lenient extended JSON receipts remain. |
| `skills/_shared/harness_profile.py#unit-01` | Partial | Registry/layer validation, type parsing, ordered knob/preset/environment maps, precedence, per-class overlays, project guards, risk floors, stamps/digests and explicit root/home inputs. Exact malformed JSON diagnostics, extended Python JSON values and malformed non-policy registry exception forms and non-default CPython integer digit-limit overrides remain. |
| `scripts/manage_installation.py#unit-03` | Partial | Execute mutation branches, custom handoffs/rebinding, relocation, selective uninstall, receipt updates, CLI options, warnings, exit/status/text/JSON output. The command inherits the earlier codec/path limitations and has no cutover CLI judge yet. |
| `skills/_shared/harness_profile.py#unit-02` | Partial | Ordered assignments, current/lenient/value, pinned instance context, validated atomic edits and empty-class cleanup. Inherits the profile reader's decode/path limitations; strengthened directory durability is RP-config-7. |
| `scripts/harness_profile.py#unit-01` | Partial | List/show/use/set/reset/diff options, text/JSON output and validated edits. Descriptions retain arbitrary Python str/repr forms, including nested object order. Inherits resolver codec/path limits; argparse abbreviation/error/help integration is not claimed. |

No listed unit was skipped. Partial units are deliberately absent from
`ported.txt`; the only new completion is the cycle interface contract.

## Shared owners and pending items

Adopted `provenant_core::paths::{resolve, expand_user}`, `provenant_core::py`
(repr, strip, equality, truthiness and number spelling),
`provenant_records::checked_json`, and supervisor `Directory::{atomic_write,
replace_link, exchange, sync}`. Deleted the local resolver, Unicode printable
range table, atomic writer/link publisher and integer-token scanner.

The private pending surface still belongs to the following owners; no such
public API is on the merged main snapshot:

- `provenant-records`: insertion-ordered JSON/map serde adapter and Python
  JSON display/canonical ASCII encoders (`pending/ordered.rs`, `pending::json_bytes`,
  `pending::json_string`), hex formatting (`hex`), schema-one equality wrapper
  (`schema_one`), plus exact Unix byte-path record text (`path_text`).
- `provenant-core`: Python surrogateescape filename ordering (`path_sort_key`)
  and Unicode decimal integer parsing (`pending::parse_int`, `DECIMAL_STARTS`).
- `provenant-supervisor`: live executable-access implementation for the
  `ExecutableAccess` host capability. Front-door process execution remains
  injected; the existing bounded runner does not preserve the source's
  inherited cargo progress and raw smoke/capture policy without an adapter.

The public `OrderedMap` is the minimum typed map needed by manifest consumers;
its implementation stays in the private ordered module, marked for records.
Per-variant policy lives in registries/ordered data; execution varies through
`InstanceOwner`, `FrontDoorHost`, and `ExecutableAccess` contracts.

## Evidence and review

The crate oracle uses `PROVENANT_PORT_PYTHON` (default `python3`, >=3.12), `-I`,
explicit environment, a bounded wait and `skip:`/required-oracle refusal policy.
Test fixture modules are imported; none evaluate source text. Paths used for
comparisons are canonicalized on macOS.

New differential evidence covers publication outcomes/refusals and retained
bytes/modes, complete skill-manager action sequences and receipts, exact
summary/JSON/exit/stderr streams, marker glob edge cases, validation order,
every instance execute result shape, profile precedence/guards/floors and
integer parsing, profile edit bytes, and a complete profile CLI edit/query
sequence. Deterministic rollback and ownership fixtures supplement those
comparisons; concurrent race/platform conformance remains a CI/integration gate.

Two native read-only reviews used the source/diagnostic and installation/
publication lenses under the requested route `codex/gpt-6.1-sol@high`.
Repairs address diagnostic order/context, arbitrary description rendering,
environment order, universal newlines and common errno rendering. Non-UTF-8
record/output findings remain explicitly partial under RP-config-6/8.
The chair owns the cross-family review; auto-merge is not armed.

Required local fmt and clippy pass; **85 crate tests pass** with the Python
differential required (34 added tests over batch 2). Results are recorded
in the PR. The separate layering fixture currently fails `config -> supervisor`:
the user explicitly requested the safe supervisor dependency, while R2's table
still allows config only core/records. The chair was notified through Fabric;
this lane cannot edit the rulebook/graph. No merge should proceed until the
rulebook owner settles that edge and exact-head CI passes.
