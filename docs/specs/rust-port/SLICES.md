# Rust cut-over slices and prune list

Rulebook: [PORTING.md version 6](PORTING.md), especially R0 and R2.
Decision: [ADR 0028](../../adr/0028-rust-port-dependencies.md). Author route:
`codex/gpt-6.1-sol@high` via Fabric. Survey date: 2026-10-23.
This is a deletion-first delivery plan, not cut-over approval or a live status board.
Workshop chairs core, records, supervisor, confinement, config, run, routing,
providers, quota, telemetry and cli. Laptop chairs workflow and all fabric-* crates.
A cross-chair slice retains those API owners; it does not transfer their crates.

## Baseline and evidence method

The survey starts at v6 commit `a08097b3` (PR #1345), merged without rebase
with fetched main `114e3331` (config #1336). The tracked inventory is **310 Python,
79 TypeScript, 65 MJS and 7 JS files**, plus the live `scripts/utility-schema.cjs`
and extensionless Python entries. Crate Rust totals **79,615 physical lines**
after that merge. Earlier “about 73k” estimates precede config batch 3.
`derive_graph.py --check` reports existing drift after that merge: only
`workspace:command-construction-lint.source_files` differs (new config Rust
files); the source digest, all 349 node IDs and other graph fields agree.
The serial graph owner must regenerate that list; this lane's write scope
does not include generated files.
`ported.txt` is an inert-unit ledger, not proof a runtime owner changed.
The per-crate PORT.md/PORT_STATUS.md reports mark much of the existing Rust
partial; modules below are reusable material, never presumed complete ports.

Reproduce the evidence from the repository root (quoted globs are intentional):

```sh
git ls-files '*.py' '*.mjs' '*.ts' '*.js'
git ls-files 'scripts/*' 'skills/*' 'runtime/*' 'workflows/*'
rg -n 'python|node|\.py|\.mjs|provenant' skills/*/SKILL.md scripts/hooks scripts/configure-claude-hooks.py
rg -n 'OWNERS|PREFIXED|LEASE_VERBS|Route::Mcp|command ==' tools/front-door/src/main.rs
rg -n 'register\(|fabric_' runtime/fabric/src/server.ts runtime/fabric/src/tool-schemas.ts
rg -n 'python_repr|python_strip|decimal_digit|surrogate|trim_ecmascript' tools/front-door/crates
```

Read `graph.json`'s `source_imports` in reverse for import callers; supplement
with `git grep -n -F -e '<full path>' -e '<filename>'` across tracked files,
including shell wrappers, package scripts, hooks, skill references and tests.
The caller manifests below record the conservative union of these searches.
A reference may be an assertion or instruction rather than an executable caller;
keeping it in the list prevents source-text tests and docs hiding stale paths.
Generated graph/inventory, port ledgers and archived historical prose are not
active callers. Archive/history references need no invocation rewrite.

**Unverified** applies to nonliteral/dynamic loader expressions, undocumented
external direct invocations and browser/installed-workflow host bindings.
For each source in `loader_review`, the lane must resolve its expressions before
claiming deletion; a text-search absence is not proof against dynamic loading.
The manifests are an audited starting closure, not a claim that dynamic edges
can be proved absent statically. Repeat the queries on the actual cut-over head.
There is no top-level `hooks/`, `scripts/detect.mjs` or `scripts/live.mjs` on this
baseline: hooks are under `scripts/hooks/` and configure-claude-hooks.py;
UI entry wrappers are under `skills/ui-ux-design/scripts/` and `runtime/ui-*`.

## Slice 0 — prune without a Rust port

Do this first, in one small PR. This lane only documents the deletions.
There are **three source files (208 lines)** and roughly **190 private test
lines** to remove; preserve the two policy assertions in the mixed projection
test. No runtime algorithm needs replacement. Direct user invocation of the
unused selector is **unverified**; the chair settles that R0 disposition before
executing its deletion, rather than inventing an external caller.

| Delete now | Evidence and edits needed in the same prune PR |
|---|---|
| `skills/deliver/scripts/select_security_evidence.py` (77), `tests/test_security_evidence_selection.py` (28) | `git grep -n select_security_evidence` outside generated graph/inventory finds only the test at lines 8, 12, 20 and 26. No front-door, skill, hook, package or CI invocation found. `reference_runs.py:82,223` and `delivery_validation_security.py` consume `config/security-evidence.json` directly: keep that config and those policy consumers. Graph `skills/deliver/scripts/select_security_evidence.py#unit-01` becomes deleted, not ported. |
| `scripts/lib/projection.py` (70), private helper assertions in `tests/test_projection.py` (about 130 of 162) | `git grep -n -e scripts.lib.projection -e projection.py` finds only that test, `tests/test_pytest_console_entrypoint.py:11`, ledgers and the inert Rust oracle in `C/cli/src/projection.rs:437`. No product caller. Remove the console collection-list entry and reconcile `MAINTAINING.md:51,75`, which calls this file a runtime-graph/delivery drift gate; retain its actual risk-policy guarantee and correct stale wording in that prune PR. Keep `test_architecture_risk_projection_matches_the_runtime_policy` and `test_internal_data_alone_stays_routine_but_consequences_escalate` as CI policy assertions, with local parsing; they are not tests of the dead projection API. Drop Rust `cli::projection` and its oracle too (see inert list). Graph `scripts/lib/projection.py#unit-01` is retirement. |
| `tools/front-door/crates/workflow/tests/generate_policy_fixtures.py` (61) | `git grep -n generate_policy_fixtures` finds no caller in CI, Cargo, skills or scripts. Its committed `workflow/tests/fixtures/{review_ladder,review_terminal}.json` are loaded by `include_str!` in `review_ladder.rs:153` and `review_terminal.rs:342,355,372`. Keep those fixtures and Rust tests. No regeneration contract is declared beyond the unused script's own docstring. |
| `tests/test_delivery_validator_structure.py` (32) | Entire test parses AST and enforces `_shared` import placement; it checks a private module layout, not validation output or records. Keep all delivery policy/CLI/hash assertions elsewhere. |


**Already gone:** ADR 0028's AST move-comparison helper and its test have no
tracked paths here: `git ls-files | rg -i '(ast|move|compare)'` finds no such
helper; the wording search finds only ADR 0028. Count zero, do not invent a
retirement PR or recreate a Rust comparison tool.

**Do not prune now:**

- `tests/test_shared_library_bootstrap.py` (117) and
  `tests/test_import_path_contract.py` (65) still guard live Python entrypoints
  through dynamic discovery/import probes. They need no Rust transliteration,
  but retain them through the mixed transition until 48 or equivalent J1
  installed-entry coverage exists.
- `scripts/static-security-check.py` is called by `scripts/check-harness:200`
  and `scripts/gate`; R6 keeps it in Python CI until no tracked Python remains.
  Its tests stay. Rust unsafe/command-construction controls are separate work.
- `scripts/lint-timebomb-fixtures.py` is called by check-harness:201; keep it
  while Python/TS test targets exist. Slice 07 retargets the textual Rust date
  policy without porting Python AST analysis.
- `F/python-owner.ts` is marked `dropped` in graph.json but catalogue/runtime
  imports still call it. Delete it in 33 when Rust APIs replace the callers.
- `O/check_herdr_cli.py` has no known product-script caller, but its contract
  test includes an opt-in installed Herdr probe and `herdr-panes.md` remains
  linked. External invocation is **unverified**; 47 requires chair disposition.
- `scripts/task_classifier.py` is live: model_route.py:80 loads it and
  model_route_cli.py:219 calls `.classify`. Delete with routing (14), not 0.
- Workflow/quota/fabric-store oracle scripts have live Rust test callers;
  core's Unicode generator is documented. Remove them with owner/oracle exit,
  not as alleged orphans. The UI browser and screenshot assets have live loaders.

A native read-only subagent checked the selector, projection, generator,
Herdr, classifier and CI holdouts against skills, hooks, CI, package scripts,
Cargo and dynamic references before this document's PR. Its missed-caller
finding is reflected above: removing the projection test also changes the
pytest console collection list. No cross-family review is claimed.

## How to execute the ordered slices

The table and cards are in dependency order. Among ready leaves, prefer the
largest deletion for the smallest remaining integration effort. Writing and
review/session leaves precede execution custody and Fabric; Fabric runtime and
MCP have distinct cut-overs. `00` is slice 0. Each numbered row is one writer
lane, budgeted **one to two lane-days**, after its owner APIs and judge cases
exist. Figures are planning budgets, not measurements of completed ports.
The two foundation rows export missing APIs rather than create another inert
wave: each is tied to its immediate command consumers and blocks them.
Before 01, verify the already-landed `workspace:manifest-and-digest` and
`workspace:front-door-syscalls` entries in `ported.txt`: the current workspace
and Python builder already hash crate sources/manifests/lockfile, and the
binary already imports supervisor. Their reuse is a prerequisite, not work
deferred to 06. Slice 06 removes the Python builder/checker only after Rust
installation owns those operations. If stale-library detection fails this
preflight, repair that prerequisite before linking any additional library.

Judge-gap and API-design work is a prerequisite, not free work hidden in these
budgets. Large uncovered areas (especially UI browser/live, delivery, run and
Fabric status/graph) are **unverified estimates**: the chair must split on
independently executable modes if a lane cannot fit two days. Do not launch an
oversized file-shaped transliteration to honour a line estimate. Re-estimate
after the first three cut-overs; do not replace README's 162-day graph estimate
with an unsupported speed-up claim.

Every cut-over PR switches the verb/entry and **all listed callers**, removes
the old algorithm and its private tests, and leaves no runtime fallback.
An unmigrated Python importer must call the new Rust operation through a
narrow typed `provenant` owner subcommand in that same PR; it cannot import a
Rust library. These temporary call adapters contain no policy/codec/state
owner and are removed with the caller's later slice. Name/capture these owner
operations in J1 before implementing them; they are proposed internal APIs,
not existing supported verbs. Never introduce the inverse Rust→Python/TS
bridge. If such an adapter is larger than switching the caller, move the
caller into the slice and re-budget it. Only one owner touches live state.

Legacy path wrappers may exec the Rust front door only when the invocation
actually executes a shebang. An explicit `python3 path.py`/`node file.mjs`
caller must change, including installed hooks/settings. Independent installed
caller fixtures prove it. Unknown external invocations remain **unverified**
and block deletion of a supported entry until the chair settles migration;
a Python compatibility shim is unfinished work, not the end state.

Tests listed per slice are **retirement buckets**, not permission to drop a
mixed file before replacing its contract assertions. Delete monkeypatch,
private-helper, interpreter/import and optional differential tests without
translating them. Move policy assertions that matter to Rust unit tests or
independent black-box cases; retain J1, J2, J3 and lane_conformance runners and
fixtures even when written in Python. Rewire their fixture builders off deleted
modules. `tests/test_record_schemas.py` currently imports old flows; each writer
slice replaces those flows with explicit candidate CLI invocations. A schema
file alone is not a passing writer judge.

Path abbreviations used only to keep the inventories readable:
`P/` = `scripts/`; `O/` = `skills/orchestrate/scripts/`;
`D/` = `skills/deliver/scripts/`; `F/` = `runtime/fabric/src/`;
`C/` = `tools/front-door/crates/`. All are repository-relative paths.
Graph shorthand `path#{interface,unit-01…03}` means that exact interface and
all numbered units in the inclusive range; there are no inferred node IDs.
A graph gap is explicit for small/JS/wrapper files not represented as units.
Read graph source spans before marking any unit complete.


| Slice | Source + test lines deleted (approx.) | Lane-days | Chair / crates | Prerequisites |
|---|---:|---:|---|---|
| 00 Prune | 208 + ~190 | 0.25 | Laptop + Workshop | None |
| 01 Record codecs and YAML readers | 0 + 0 | 1.5 | Workshop / records, core | 00 |
| 02 Safe host operations and lane custody | 0 + 0 | 2 | Workshop / supervisor, cli | 01 |
| 03 Writing linters | 799 + 119 | 1 | Laptop + Workshop / workflow, cli | 00,01,02 |
| 04 Hooks and resolved profiles | 1,559 + 1,109 | 1.5 | Workshop / config, cli | 01,02 |
| 05 Managed installation | 2,178 + 4,725 | 2 | Workshop / config, cli | 04 |
| 06 Front-door builder and install diagnostics | 762 + 1,176 | 1.5 | Workshop / config, cli | 05 |
| 07 Harness checks and textual date lint | 736 + 634 | 1.5 | Workshop + Laptop / cli, workflow, records | 01,06 |
| 08 Git evidence and PR settlement | 652 + 532 | 1.5 | Workshop + Laptop / cli, workflow | 02,06 |
| 09 Review policy and portable workflow | 1,190 + 632 | 1.5 | Laptop + Workshop / workflow, cli | 01,02 |
| 10 Session, pause and mission helpers | 1,646 + 1,608 | 2 | Laptop + Workshop / workflow, cli | 09 |
| 11 Quota and schedules | 3,125 + 2,963 | 2 | Workshop / quota, cli | 01,02,04 |
| 12 Adapter profiles and capabilities | 1,149 + 437 | 1.5 | Workshop / providers, routing, cli | 02,04 |
| 13 Route catalogue and live discovery | 1,087 + 69 | 1.5 | Workshop / routing, cli | 11,12 |
| 14 Route decisions, pools and classifier | 3,914 + 5,883 | 2 | Workshop / routing, cli | 13 |
| 15 Telemetry ledger and ingest | 2,130 + 3,543 | 2 | Workshop / telemetry, cli | 02,04,11,14 |
| 16 Telemetry summaries and host snapshots | 706 + 284 | 1.5 | Workshop / telemetry, cli | 15 |
| 17 Confinement and custody policy | 2,643 + 718 | 2 | Workshop + Laptop / confinement, workflow, cli | 02,05,08 |
| 18 Execution planning and preflight | 584 + 158 | 1.5 | Workshop + Laptop / providers, run, workflow, cli | 14,17 |
| 19 Attempt records and observer boundary | 3,202 + 1,948 | 2 | Workshop / records, supervisor, run, cli | 02,12,18 |
| 20 Dispatch owner | 2,483 + 7,529 | 2 | Workshop / run, providers, cli | 15,18,19 |
| 21 Batch and run control | 2,912 + 3,677 | 2 | Workshop / run, cli | 20 |
| 22 Direct provider entry and detached workers | 4,949 + 12,616 | 2 | Workshop / providers, supervisor, run, cli | 20,21 |
| 23 Utility operations | 257 + 154 | 1.5 | Workshop / cli, run, records | 14,20,22 |
| 24 Finalization, memory and log reduction | 1,198 + 2,002 | 1.5 | Workshop / run, cli | 19,21 |
| 25 Delivery validation policy | 2,739 + 2,078 | 2 | Laptop + Workshop / workflow, records, cli | 09,17,19 |
| 26 Receipt mutation and reference builders | 2,246 + 1,291 | 2 | Laptop + Workshop / workflow, cli | 25 |
| 27 Merged-delivery continuity and checkpoints | 437 + 904 | 1.5 | Laptop + Workshop / workflow, cli | 08,26 |
| 28 Worktrees | 1,227 + 1,018 | 2 | Workshop / cli, confinement | 08,17,25 |
| 29 Retention cleanup and explicit reaping | 1,211 + 1,526 | 2 | Workshop / cli, supervisor | 21,24,25,28 |
| 30 Delivery telemetry and retrospectives | 682 + 0 | 1.5 | Workshop / telemetry, cli | 16,26,27 |
| 31 Fabric store commands | 1,314 + 639 | 2 | Laptop + Workshop / fabric-store, fabric-runtime, cli | 01,02,06 |
| 32 Fabric identity and retained run reads | 2,777 + 2,382 | 2 | Laptop + Workshop / fabric-runtime, cli; supervisor API owned by Workshop | 19,21,31 |
| 33 Fabric TS execution facade | 2,589 + 3,583 | 2 | Laptop + Workshop / fabric-runtime, cli | 14,20,21,22,31,32 |
| 34 Fabric named sessions and resume | 592 + 677 | 1.5 | Laptop + Workshop / fabric-runtime, fabric-store, cli | 33 |
| 35 Bounded graphs and landing | 1,840 + 1,054 | 2 | Laptop + Workshop / fabric-runtime, fabric-store, cli | 28,31,33,34 |
| 36 Fabric federation and Git landing transport | 3,168 + 4,197 | 2 | Laptop + Workshop / fabric-runtime, cli | 08,16,28,32,33,35 |
| 37 MCP server cut-over | 997 + 1,406 | 2 | Laptop + Workshop / fabric-mcp, cli | 31,32,33,34,35,36 |
| 38 Fabric CLI and Node exit | 2,261 + 4,146 | 1.5 | Laptop + Workshop / cli, fabric-runtime; config API owned by Workshop | 37 |
| 39 UI evidence text engine | 2,759 + 0 | 1.5 | Laptop + Workshop / workflow, cli | 03,01 |
| 40 UI static HTML and CSS detection | 1,486 + 0 | 2 | Laptop + Workshop / workflow, cli | 39 |
| 41 UI detector browser and screenshot transport | 658 + 0 | 2 | Laptop + Workshop / workflow, cli; supervisor API owned by Workshop | 02,40 |
| 42 UI detector entry and generated bundle | 6,321 + 708 | 1.5 | Laptop + Workshop / workflow, cli | 39,40,41 |
| 43 UI live source and context | 2,565 + 2,433 | 1.5 | Laptop + Workshop / workflow, confinement, cli | 17,40 |
| 44 UI live journal and recovery | 732 + 0 | 2 | Laptop + Workshop / workflow, cli | 43 |
| 45 UI live process and HTTP owner | 2,126 + 673 | 2 | Laptop + Workshop / workflow, cli; supervisor API owned by Workshop | 02,41,44 |
| 46 UI browser interaction and live entry | 8,288 + 1,817 | 2 | Laptop + Workshop / workflow, cli | 42,45 |
| 47 Installed workflow accelerators and tracker hook | 2,310 + 499 | 2 | Laptop + Workshop / workflow, cli | 09,26,27,28,33 |
| 48 Public release checks and shared-source exit | 1,028 + 2,278 | 1.5 | Workshop / cli, core; workflow caller API owned by Laptop | 06,07,08,10,14,17,24,29,38,46,47 |
| 49 Retire runtime-only tests and Node infrastructure | 761 + 1,503 | 1 | Laptop + Workshop / workflow, fabric-*; Workshop cli/config hooks | 38,42,46,47,48 |

Line counts are physical current-source lines including comments and blank
lines, not “net Rust saved”. Whole test buckets are approximate upper bounds;
retained black-box assertions reduce the actual count. No source is counted
twice. YAML files/config data are migrated in 01, not counted as deleted
Python. Foundation slices have zero legacy-source deletion. Shared roots and
host identity delete at 48 after adapters have moved every old importer.


### 01 — Record codecs and YAML readers

**Owner:** Workshop; crates `records, core`. **Budget:** 1.5 lane-days. **Prerequisites:** 00; owner APIs A1,A2.

**Commands, entries and tools:** Owner APIs used first by check/install below; no new user verb or MCP tool.

**Existing Rust:** records::checked_json, delivery_run_shape, layout and core::{jsonc,bytes}: reuse. Implement only contract-required encoders; do not expand generic Python compatibility.

**Contract / judges:** J2 unchanged schemas; canonical digest bytes, integer ranges, optional/null fields; ADR 0028 external-YAML subset and owned-data JSON migration. **Missing cases / blockers:** J2 managed manifest/profile/receipt schemas and external YAML refusal fixtures; capture every tracked external group and migrate all owned YAML readers together.

**Graph units:** `records:external-yaml-subset`; `records:yaml-data-to-json`.

**Delete source (0 lines):** None.

**Retire tests (0 lines, subject to the mixed-test rule):** None.

**Caller/reference closure to switch:** None.

**Manual invocation/ownership evidence:** All reader paths in records:yaml-data-to-json source_files, including model_route.py runtime compatibility data, check_harness.py/eval fixtures and runtime/ui-*/provenance_components.yaml. External issue forms, agents/openai.yaml and SKILL.md frontmatter remain YAML.

### 02 — Safe host operations and lane custody

**Owner:** Workshop; crates `supervisor, cli`. **Budget:** 2 lane-days. **Prerequisites:** 01; owner APIs A3.

**Commands, entries and tools:** `provenant lane` keeps the accepted S2 owner; safe byte/stream/Git/publication APIs for later verbs. No MCP.

**Existing Rust:** supervisor::{sys,fs,bounded,capture,identity,ps,process_info,worker_liveness}: reuse safe handles and accepted S2; simplify pending policy copies. Existing root lane remains active, library portions inert.

**Contract / judges:** Linux/macOS lane_conformance plus J2 launch/exit/owner records; R5 never kill on silence or failed bookkeeping. **Missing cases / blockers:** Raw dual-stream bounded Git runner with stdin, streaming output/cleanup, entropy, no-replace recovery; move remaining binary lane unsafe and enable R7 command lint. No claim existing bounded text runner supplies these.

**Graph units:** reuse completed `workspace:front-door-syscalls` and its already-landed `workspace:manifest-and-digest` prerequisite; complete `workspace:front-door-unsafe-guard` and `workspace:command-construction-lint`.

**Delete source (0 lines):** None.

**Retire tests (0 lines, subject to the mixed-test rule):** None.

**Caller/reference closure to switch:** None.

**Manual invocation/ownership evidence:** Every later effectful slice uses this API. tools/front-door/src/{lane,exec,resolve}.rs own existing binary syscalls; library API adoption must not leave direct Command construction in other crates.

### 03 — Writing linters

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, cli`. **Budget:** 1 lane-days. **Prerequisites:** 00,01,02; owner APIs A1,A2.

**Commands, entries and tools:** Direct style_lint.py, check_academic_style.py, lint_legal_style.py → proposed `provenant writing lint|academic|legal` (new spellings need J1 entry-point capture). No MCP.

**Existing Rust:** workflow::style_lint: reuse pattern/span logic; replace Python PatternRule overlays with typed Rust matchers.

**Contract / judges:** J1 direct invocations, file/line diagnostics and exits; R0 retains output, not Python regex objects. **Missing cases / blockers:** No J1 writing cases; add Markdown/LaTeX exclusions, overlay patterns, UTF-8 coordinates and malformed input.

**Graph units:** `skills/legal-writing/scripts/lint_legal_style.py#{unit-01…02}`; `skills/natural-writing/scripts/check_academic_style.py#{unit-01}`; `skills/natural-writing/scripts/style_lint.py#{unit-01…02}`.

**Delete source (799 lines):** `skills/legal-writing/scripts/lint_legal_style.py`, `skills/natural-writing/scripts/check_academic_style.py`, `skills/natural-writing/scripts/style_lint.py`.

**Retire tests (119 lines, subject to the mixed-test rule):** `tests/test_legal_writing_portability.py`, `tests/test_specialist_cleanup.py`.

**Caller/reference closure to switch:** `MAINTAINING.md`, `skills/legal-writing/SKILL.md`, `skills/legal-writing/references/australian-english-house-style.md`, `skills/legal-writing/references/forbidden-patterns.md`, `skills/legal-writing/references/validation-checklists.md`, `skills/natural-writing/references/academic-prose.md`, `tests/test_legal_writing_portability.py`, `tests/test_specialist_cleanup.py`, `C/workflow/src/style_lint.rs`, `C/workflow/tests/oracle.py`.

### 04 — Hooks and resolved profiles

**Owner:** Workshop; crates `config, cli`. **Budget:** 1.5 lane-days. **Prerequisites:** 01,02; owner APIs A1,A2,A3,A4.

**Commands, entries and tools:** `profile`; installed Claude lane-safety/routing/utility hooks; Codex skill overrides. Proposed `provenant hook ...` replaces interpreter commands. No MCP.

**Existing Rust:** config::{codex,profile_command,profiles}; cli::{lane_safety,harness}: reuse policy, simplify parser/entry.

**Contract / judges:** J1 profile/help and hook stdin/stdout/exits/env; J2 profile/config precedence; managed JSON/TOML preserves unowned keys. **Missing cases / blockers:** Add hook invalid/lone-surrogate inputs, empty/absent env, repeated flags and reconciliation; installed hook fixture must execute the Rust verb.

**Graph units:** `P/configure-claude-hooks.py#{unit-01}`; `P/configure-codex-skill-overrides.py#{unit-01}`; `P/harness_profile.py#{unit-01}`; `P/lane_safety_guard.py#{unit-01…02}`; `P/routing_context_hook.py#{unit-01}`; `P/utility_output_hook.py#{unit-01}`; `skills/_shared/harness_profile.py#{unit-01…02}`.

**Delete source (1,559 lines):** `P/configure-claude-hooks.py`, `P/configure-codex-skill-overrides.py`, `P/harness_profile.py`, `P/lane_safety_guard.py`, `P/routing_context_hook.py`, `P/utility_output_hook.py`, `skills/_shared/harness_profile.py`.

**Retire tests (1,109 lines, subject to the mixed-test rule):** `tests/test_harness_profile.py`, `tests/test_lane_safety.py`, `tests/test_utility_hook.py`.

**Caller/reference closure to switch:** `docs/runbooks/model-routing.md`, `docs/specs/harness/profiles.md`, `P/install-harness`, `P/model_route.py`, `P/provenant-profile`, `skills/orchestrate/references/lane-safety.md`, `O/batch_run.py`, `O/dispatch_preflight.py`, `O/dispatch_run.py`, `O/provider_exec.py`, `tests/test_harness_profile.py`, `tests/test_install_harness.py`, `tests/test_lane_safety.py`, `tests/test_task_classifier.py`, `tests/test_utility_hook.py`, `C/cli/src/lane_safety.rs`, `C/config/src/codex.rs`, `C/config/src/profile_command.rs`, `C/config/src/profiles.rs`.

**Manual invocation/ownership evidence:** configure-claude-hooks.py writes interpreter-prefixed commands into installed settings. Installer reconciliation must rewrite those stored strings, not just source references.

### 05 — Managed installation

**Owner:** Workshop; crates `config, cli`. **Budget:** 2 lane-days. **Prerequisites:** 04; owner APIs A1,A2,A3,A4.

**Commands, entries and tools:** install-harness/install-skills/install-workflows and manage_installation.py direct modes → Rust install subcommands; no MCP.

**Existing Rust:** config::{installation,filesystem,managed_skills,manifest,instance,workflows}: reuse policy from main #1336, finish effects and simplify Python object readers.

**Contract / judges:** J1 direct install argv/help/exits; J2 managed manifest/pointers; isolated install, no overwrite of custom skills, rollback on both Macs. **Missing cases / blockers:** Manifest schema, staged symlink races, unowned settings and rollback after partial publish, unchanged-source digest/stale library cases.

**Graph units:** `P/install-workflows#{unit-01}`; `P/instance_installation.py#{interface,unit-01…03}`; `P/manage_installation.py#{unit-01…03}`; `P/managed_installation_manifest.py#{unit-01}`; `P/write-product-root-pointer.py#{unit-01}`.

**Delete source (2,178 lines):** `P/install-workflows`, `P/instance_installation.py`, `P/manage_installation.py`, `P/managed_installation_manifest.py`, `P/write-product-root-pointer.py`.

**Retire tests (4,725 lines, subject to the mixed-test rule):** `tests/test_install_harness.py`, `tests/test_install_skills.py`, `tests/test_instance_installation.py`, `tests/test_quickstart_bootstrap.py`.

**Caller/reference closure to switch:** `.github/workflows/ci.yml`, `MAINTAINING.md`, `README.md`, `docs/ARCHITECTURE.md`, `docs/adr/0019-installed-file-class-ownership.md`, `P/check-provenant-install.py`, `P/install-harness`, `P/install-skills`, `tests/test_import_path_contract.py`, `tests/test_install_skills.py`, `tests/test_instance_installation.py`, `tests/test_model_route_pools.py`, `tests/test_product_root_resolver.py`, `C/cli/src/install.rs`, `C/config/src/installation.rs`, `C/config/src/instance.rs`, `C/config/src/managed_skills.rs`, `C/config/src/manifest.rs`, `C/config/src/pending.rs`, `C/config/src/workflows.rs`.

### 06 — Front-door builder and install diagnostics

**Owner:** Workshop; crates `config, cli`. **Budget:** 1.5 lane-days. **Prerequisites:** 05; owner APIs A1,A2,A3,A4.

**Commands, entries and tools:** install-provenant-command.py, check-provenant-install.py; root/help/stale advisories remain front-door contracts. No MCP.

**Existing Rust:** config::{front_door,command}; cli::{install,stale_checkout}; core root/path modules: reuse; preserve source marker.

**Contract / judges:** J1 root/help and install diagnostics; stale-checkout and install drift fixtures; R2 entire workspace digest before first linked library. **Missing cases / blockers:** Add J1 installed diagnostics, byte paths, corrupt marker and stale library/manifests/lockfile; check no SQLite/thread/provider on help/root.

**Graph units:** `P/check-provenant-install.py#{interface,unit-01}`; `P/install-provenant-command.py#{unit-01}`; `P/lib/front_door_build.py#{unit-01}`; `P/lib/stale_checkout.py#{interface,unit-01}`. Reuse prerequisite `workspace:manifest-and-digest` is already ported; do not claim or dispatch it again.

**Delete source (762 lines):** `P/check-provenant-install.py`, `P/install-provenant-command.py`, `P/lib/front_door_build.py`, `P/lib/stale_checkout.py`.

**Retire tests (1,176 lines, subject to the mixed-test rule):** `tests/test_check_provenant_install.py`, `tests/test_front_door_build.py`, `tests/test_install_provenant_command.py`, `tests/test_stale.py`.

**Caller/reference closure to switch:** `.github/workflows/ci.yml`, `docs/adr/0027-rust-front-door.md`, `docs/runbooks/front-door.md`, `docs/specs/harness/rust-supervisor-tests.md`, `docs/specs/harness/rust-supervisor.md`, `runtime/fabric/README.md`, `P/check-harness`, `P/fabric_hosts.py`, `P/install-harness`, `tests/support/front_door.py`, `tests/test_check_provenant_install.py`, `tests/test_front_door_build.py`, `tests/test_install_harness.py`, `tests/test_install_provenant_command.py`, `tests/test_stale.py`, `C/cli/src/install.rs`, `C/config/src/command.rs`, `C/config/src/front_door.rs`.

**Manual invocation/ownership evidence:** scripts/install-harness and scripts/install-skills invoke build/install helpers; workspace digest is already implemented in scripts/lib/front_door_build.py and mirrored in config::front_door, so assert stale-library detection before linking any new library, rather than waiting for this deletion row.

### 07 — Harness checks and textual date lint

**Owner:** Workshop (cli); Laptop (workflow); crates `cli, workflow, records`. **Budget:** 1.5 lane-days. **Prerequisites:** 01,06; owner APIs A1,A2,A3.

**Commands, entries and tools:** `check`, `gate`, `lint`; check_target.py; Rust test date lint. No MCP. Shell composites exec the single front door.

**Existing Rust:** cli::{harness,timebomb}; workflow style policy: reuse; no Python AST port.

**Contract / judges:** J1 check/gate/lint cases; external YAML subset and link/skill checks. Static security and Python/TS fixture-date scans remain CI Python. **Missing cases / blockers:** Add real bounded check success/refusal beyond help, route/issue-form/link semantics; Rust changed-test discovery/date lint fixtures.

**Graph units:** `P/check_harness.py#{unit-01…02}`; `P/check_target.py#{unit-01}`; `P/provenant_lint.py#{unit-01}`; `cli:rust-test-date-lint`.

**Delete source (736 lines):** `P/check_harness.py`, `P/check_target.py`, `P/provenant_lint.py`.

**Retire tests (634 lines, subject to the mixed-test rule):** `tests/test_command_docs.py`, `tests/test_harness_document_contract.py`, `tests/test_provenant_lint.py`, `tests/test_skill_eval_fixtures.py`.

**Caller/reference closure to switch:** `.github/workflows/ci.yml`, `docs/adr/0013-thin-provenant-cli.md`, `docs/adr/0027-rust-front-door.md`, `docs/runbooks/lint.md`, `docs/specs/harness/rust-supervisor.md`, `P/check-harness`, `skills/skill-craft/SKILL.md`, `tests/golden/cases/help/help-case-01.json`, `tests/golden/cases/usage/usage-alias-01.json`, `tests/test_harness_document_contract.py`, `tests/test_provenant_lint.py`, `tests/test_rust_port_graph.py`, `tests/test_skill_eval_fixtures.py`, `tests/test_stale.py`, `C/cli/src/harness.rs`, `tools/front-door/src/check.rs`, `tools/front-door/src/help/general.txt`, `tools/front-door/src/help/lint.txt`, `tools/front-door/src/main.rs`, `tools/front-door/tests/common/mod.rs`, `tools/front-door/tests/front_door.rs`.

**Manual invocation/ownership evidence:** scripts/check-harness, scripts/gate and .github/workflows/ci.yml are CI entry/composite callers. Preserve static security/date scans while their source globs remain.

### 08 — Git evidence and PR settlement

**Owner:** Workshop (cli); Laptop (workflow); crates `cli, workflow`. **Budget:** 1.5 lane-days. **Prerequisites:** 02,06; owner APIs A1,A2,A3,A5.

**Commands, entries and tools:** `pr checks`; git_evidence.py and git_evidence_packet.py direct entry points → Rust evidence commands. No MCP.

**Existing Rust:** cli::{git_evidence,pr_checks}; workflow::git_evidence_packet: reuse pure framing/settlement, supply raw supervisor adapter.

**Contract / judges:** J1 pr and direct evidence argv/stdout/exits; exact compact header/diff bytes, closed Git environment. **Missing cases / blockers:** Offline gh ruleset/classic-required checks, moving head and empty conclusion, stdin/non-UTF8 diff, filters, capture limits and failed atomic write.

**Graph units:** `P/git_evidence.py#{unit-01}`; `P/provenant_pr.py#{unit-01}`; `O/git_evidence_packet.py#{unit-01}`.

**Delete source (652 lines):** `P/git_evidence.py`, `P/provenant_pr.py`, `O/git_evidence_packet.py`.

**Retire tests (532 lines, subject to the mixed-test rule):** `tests/test_git_evidence.py`, `tests/test_provenant_pr.py`.

**Caller/reference closure to switch:** `P/provenant-pr`, `P/public_release_check.py`, `D/software_delivery_validation.py`, `skills/implement/scripts/bind_merged_delivery.py`, `O/dispatch_run.py`, `O/run_controls.py`, `tests/test_git_evidence.py`, `tests/test_provenant_pr.py`, `tests/test_public_release_check.py`, `C/workflow/tests/oracle.py`.

### 09 — Review policy and portable workflow

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, cli`. **Budget:** 1.5 lane-days. **Prerequisites:** 01,02; owner APIs A1,A2,A3,A6.

**Commands, entries and tools:** Direct review_plan/review_terminal/portable_workflow/run_gates entry points → Rust workflow verbs; review_ladder/panel become library policy. No MCP.

**Existing Rust:** workflow::{review_ladder,review_panel,review_terminal,portable_workflow,run_gates}: reuse policy, remove injected Python globals.

**Contract / judges:** J1 direct entry catalogue; J2 portable workflow/review records and digest bytes. **Missing cases / blockers:** Add panel shortfalls, duplicated family, malformed registry, invalid terminal review and gate failure cases; J1 has no direct workflow success fixtures.

**Graph units:** `skills/_shared/portable_workflow.py#{unit-01}`; `skills/_shared/review_ladder.py#{unit-01}`; `skills/_shared/review_panel.py#{unit-01}`; `skills/_shared/review_plan.py#{unit-01…02}`; `skills/_shared/review_terminal.py#{unit-01}`; `skills/_shared/run_gates.py#{unit-01}`.

**Delete source (1,190 lines):** `skills/_shared/portable_workflow.py`, `skills/_shared/review_ladder.py`, `skills/_shared/review_panel.py`, `skills/_shared/review_plan.py`, `skills/_shared/review_terminal.py`, `skills/_shared/run_gates.py`.

**Retire tests (632 lines, subject to the mixed-test rule):** `tests/test_review_ladder.py`, `tests/test_review_panel.py`, `tests/test_review_terminal.py`.

**Caller/reference closure to switch:** `docs/specs/harness/profiles.md`, `schemas/README.md`, `skills/_shared/delivery_run_invariants.py`, `D/delivery_run_shape.py`, `D/delivery_validation_common.py`, `skills/orchestrate/references/verification.md`, `O/run_dir_finalize.py`, `tests/test_import_path_contract.py`, `tests/test_install_skills.py`, `tests/test_pytest_console_entrypoint.py`, `tests/test_review_ladder.py`, `tests/test_review_panel.py`, `tests/test_review_terminal.py`, `tests/test_run_dir_finalize.py`, `C/workflow/tests/oracle.py`.

### 10 — Session, pause and mission helpers

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, cli`. **Budget:** 2 lane-days. **Prerequisites:** 09; owner APIs A1,A2,A3,A6.

**Commands, entries and tools:** session state/context audit/compact-reinject/cleanup scripts; bootstrap-autopilot and validate_idle_pause → Rust session/mission verbs. No MCP.

**Existing Rust:** workflow::{bootstrap,idle_pause,compact_reinject}: reuse; audit/state/cleanup traversal needs implementation.

**Contract / judges:** J1 installed direct/hook entry points; retained handoff/state/mission files and pause/cleanup refusals. **Missing cases / blockers:** Add independent hook launch, missing/malformed handoff, freshness, pause fail-closed and protected cleanup; no J1 session catalogue yet.

**Graph units:** `skills/autopilot/scripts/bootstrap-autopilot.py#{unit-01…02}`; `skills/autopilot/scripts/validate_idle_pause.py#{unit-01}`; `skills/session/scripts/cleanup_run_artifacts.py#{unit-01}`; `skills/session/scripts/compact_reinject.py#{unit-01}`; `skills/session/scripts/context_audit.py#{unit-01}`; `skills/session/scripts/state_check.py#{unit-01}`.

**Delete source (1,646 lines):** `skills/autopilot/scripts/bootstrap-autopilot.py`, `skills/autopilot/scripts/validate_idle_pause.py`, `skills/session/scripts/cleanup_run_artifacts.py`, `skills/session/scripts/compact_reinject.py`, `skills/session/scripts/context_audit.py`, `skills/session/scripts/state_check.py`.

**Retire tests (1,608 lines, subject to the mixed-test rule):** `tests/test_autopilot_bootstrap.py`, `tests/test_autopilot_pause.py`, `tests/test_context_audit.py`, `tests/test_context_resume.py`, `tests/test_run_artifact_cleanup.py`, `tests/test_session_state_check.py`.

**Caller/reference closure to switch:** `docs/specs/observability-console.md`, `skills/autopilot/SKILL.md`, `skills/autopilot/references/codex-operator.md`, `skills/autopilot/references/recovery-and-cadence.md`, `skills/autopilot/references/state-contract.md`, `skills/autopilot/templates/GOAL.template.md`, `skills/autopilot/templates/README.template.md`, `skills/autopilot/templates/STATE.template.md`, `skills/session/references/chair-state.md`, `skills/session/references/context-hygiene.md`, `tests/test_autopilot_bootstrap.py`, `tests/test_autopilot_pause.py`, `tests/test_context_audit.py`, `tests/test_run_artifact_cleanup.py`, `tests/test_session_state_check.py`, `C/workflow/src/bootstrap.rs`, `C/workflow/src/bootstrap_defaults/STATE.md`, `C/workflow/src/bootstrap_defaults/usage.txt`, `C/workflow/tests/oracle.py`, `workflows/implement-run.js`.

### 11 — Quota and schedules

**Owner:** Workshop; crates `quota, cli`. **Budget:** 2 lane-days. **Prerequisites:** 01,02,04; owner APIs A1,A2,A3,A4,A7.

**Commands, entries and tools:** `quota` and provider quota direct script forms; no MCP until adapters projection.

**Existing Rust:** quota::{schedule,display,time}: reuse; provider probes/cache persistence are unfinished. Simplify oracle-driven int/strip machinery.

**Contract / judges:** J1 quota; J2 quota-snapshot; cache locks, reset times/DST, curl headers on stdin. **Missing cases / blockers:** Fake all five quota providers, corrupt cache/stale/empty credentials, timeout, DST folds/gaps and OAuth/key policy; existing J1 is mostly help/refusal.

**Graph units:** `P/provider_quota.py#{interface,unit-01…05}`; `P/quota_display.py#{unit-01…02}`; `P/quota_providers/agy.py#{unit-01}`; `P/quota_providers/claude.py#{unit-01}`; `P/quota_providers/codex.py#{unit-01}`; `P/quota_providers/cursor.py#{unit-01}`; `P/quota_providers/opencode.py#{unit-01}`; `P/quota_schedule.py#{unit-01}`.

**Delete source (3,125 lines):** `P/provider_quota.py`, `P/quota_display.py`, `P/quota_providers/agy.py`, `P/quota_providers/claude.py`, `P/quota_providers/codex.py`, `P/quota_providers/cursor.py`, `P/quota_providers/opencode.py`, `P/quota_schedule.py`.

**Retire tests (2,963 lines, subject to the mixed-test rule):** `tests/test_provider_quota.py`, `tests/test_quota_display.py`, `tests/test_quota_schedule.py`.

**Caller/reference closure to switch:** `THIRD_PARTY_NOTICES.md`, `apps/quota-menubar/Fixtures/make_fixtures.py`, `apps/quota-menubar/README.md`, `apps/quota-menubar/Sources/QuotaMenuBar/QuotaModel.swift`, `apps/quota-menubar/Sources/QuotaMenuBar/Report.swift`, `config/harness-knobs.json`, `docs/runbooks/provider-quirks.md`, `docs/specs/harness/graph-runner.md`, `docs/specs/harness/operating-model.md`, `docs/specs/harness/profiles.md`, `docs/specs/harness/rust-supervisor.md`, `schemas/README.md`, `schemas/quota-snapshot.schema.json`, `P/model_route_live.py`, `P/provenant-quota`, `P/telemetry.py`, `P/telemetry_hosts.py`, `O/exec_routing.py`, `O/provider_exec.py`, `tests/support/quota.py`, `tests/test_cf_dispatch.py`, `tests/test_dispatch_run.py`, `tests/test_provider_exec.py`, `tests/test_provider_quota.py`, `tests/test_quota_display.py`, `tests/test_quota_schedule.py`, `tests/test_record_schemas.py`, `C/quota/src/tests/oracle.py`.

### 12 — Adapter profiles and capabilities

**Owner:** Workshop; crates `providers, routing, cli`. **Budget:** 1.5 lane-days. **Prerequisites:** 02,04; owner APIs A1,A2,A3,A4,A8.

**Commands, entries and tools:** `capabilities`; provider_identity.py; seven adapter wire profiles; MCP fabric_adapters later.

**Existing Rust:** providers::{exec_contract,dispatch_contract,output_contract} and exec_contract.rs adapter builders; routing::capability: reuse registry/argv policy; simplify semantic clones.

**Contract / judges:** J1 capabilities, provider argv/env/permission transcripts; J2 applied grant/receipt schema. **Missing cases / blockers:** Codex MCP inventory and plugin/config errors, Kiro/OpenCode version refusals, all seven launch forms, exact applied effort and permission boundary.

**Graph units:** `P/provider_identity.py#{unit-01}`; `O/adapters/__init__.py#{unit-01}`; `O/adapters/agy.py#{unit-01}`; `O/adapters/claude.py#{unit-01}`; `O/adapters/codex.py#{unit-01}`; `O/adapters/copilot.py#{unit-01}`; `O/adapters/cursor.py#{unit-01}`; `O/adapters/kiro.py#{unit-01}`; `O/adapters/opencode.py#{unit-01}`; `O/capabilities.py#{unit-01…02}`.

**Delete source (1,149 lines):** `P/provider_identity.py`, `O/adapters/__init__.py`, `O/adapters/agy.py`, `O/adapters/claude.py`, `O/adapters/codex.py`, `O/adapters/copilot.py`, `O/adapters/cursor.py`, `O/adapters/kiro.py`, `O/adapters/opencode.py`, `O/capabilities.py`.

**Retire tests (437 lines, subject to the mixed-test rule):** `tests/test_capabilities.py`, `tests/test_provider_skill_boundaries.py`.

**Caller/reference closure to switch:** `docs/runbooks/model-routing.md`, `docs/runbooks/provider-quirks.md`, `docs/specs/fabric-v2.md`, `docs/specs/harness/graph-runner.md`, `docs/specs/harness/operating-model.md`, `P/check-harness`, `P/fabric_hosts.py`, `P/provider_quota.py`, `P/quota_providers/agy.py`, `P/quota_providers/claude.py`, `P/quota_providers/codex.py`, `P/quota_providers/cursor.py`, `P/quota_providers/opencode.py`, `P/telemetry.py`, `O/cf_dispatch.py`, `O/dispatch_preflight.py`, `O/fabric_records.py`, `O/provider_confinement.py`, `O/provider_exec.py`, `O/provider_observer.py`, `O/provider_output.py`, `tests/golden/cases/capabilities/capabilities-case-01.json`, `tests/golden/cases/capabilities/capabilities-case-02.json`, `tests/golden/cases/help/help-case-01.json`, `tests/golden/cases/usage/usage-alias-01.json`, `tests/test_capabilities.py`, `tests/test_cf_dispatch.py`, `tests/test_fabric_hosts.py`, `tests/test_host_identity.py`, `tests/test_install_skills.py`, `tests/test_provider_exec.py`, `tests/test_provider_quota.py`, `tools/front-door/src/help/capabilities.txt`, `tools/front-door/src/help/general.txt`, `tools/front-door/src/main.rs`, `tools/front-door/tests/common/mod.rs`.

**Manual invocation/ownership evidence:** Adapters have CLI wire contracts even when no direct Python main exists. Remaining planner callers must consume typed Rust profiles; do not silently keep Python argv builders.

### 13 — Route catalogue and live discovery

**Owner:** Workshop; crates `routing, cli`. **Budget:** 1.5 lane-days. **Prerequisites:** 11,12; owner APIs A1,A2,A3,A4,A7,A8,A9.

**Commands, entries and tools:** catalogue loading behind `route snapshot`, `route probe --listing-only`, `route routes` and `routes`; live model discovery also serves Fabric `adapters --models`. routing_context_hook caller already switched in 04. No MCP registration switch.

**Existing Rust:** routing::{catalog,discovery,live,probe,render}: reuse pure matching/discovery policy; explicit IO/cache owner.

**Contract / judges:** J1 route/routes cases and J2 model-routing/preferences; wire argv/env and catalog source provenance. **Missing cases / blockers:** Add fake live model discovery, invalid snapshot, case-collision/alias errors, stale cache and provider availability; current route-resolve is help only.

**Graph units:** `P/model_route_catalog.py#{unit-01}`; `P/model_route_live.py#{interface,unit-01…02}`; `P/model_route_probe.py#{unit-01…02}`.

**Delete source (1,087 lines):** `P/model_route_catalog.py`, `P/model_route_live.py`, `P/model_route_probe.py`.

**Retire tests (69 lines, subject to the mixed-test rule):** `tests/test_model_benchmarks.py`.

**Caller/reference closure to switch:** `P/model_route.py`, `tests/test_cf_dispatch.py`, `tests/test_model_route.py`, `C/routing/src/catalog.rs`, `C/routing/src/discovery/tests.rs`, `C/routing/src/matching/tests.rs`.

### 14 — Route decisions, pools and classifier

**Owner:** Workshop; crates `routing, cli`. **Budget:** 2 lane-days. **Prerequisites:** 13; owner APIs A1,A2,A3,A7,A8,A9.

**Commands, entries and tools:** `route resolve|select|health|cooldowns|reset|pick`, `route <pool>` and `routes`; task classifier; pool picks/rotation/preferences. No MCP until 33.

**Existing Rust:** routing::{model_route,matching,pools,preferences,cli,exec_routing}: reuse; replace callback/global cycle with immutable RouteContext.

**Contract / judges:** J1 CLI aliases/errors/env; J2 routing/preferences/health; locked rotation and applied route identity. **Missing cases / blockers:** Unknown/retired tier and effort, quota cooldown, locked preference races, malformed YAML→JSON compatibility file, no invented resolved model.

**Graph units:** `P/model_route.py#{interface,unit-01…05}`; `P/model_route_cli.py#{unit-01…02}`; `P/model_route_pools.py#{unit-01…03}`; `P/model_route_preferences.py#{unit-01…02}`; `P/task_classifier.py#{interface,unit-01}`; `O/exec_routing.py#{interface,unit-01…02}`.

**Delete source (3,914 lines):** `P/model_route.py`, `P/model_route_cli.py`, `P/model_route_pools.py`, `P/model_route_preferences.py`, `P/task_classifier.py`, `O/exec_routing.py`.

**Retire tests (5,883 lines, subject to the mixed-test rule):** `tests/test_model_route.py`, `tests/test_model_route_pools.py`, `tests/test_task_classifier.py`.

**Caller/reference closure to switch:** `config/harness-knobs.json`, `docs/adr/0023-codex-custom-providers-inline-config.md`, `docs/specs/fabric-v2.md`, `docs/specs/harness/profiles.md`, `docs/specs/harness/rust-supervisor.md`, `F/catalogue.ts`, `F/execution-input.ts`, `F/pools.ts`, `runtime/fabric/tests/catalogue.test.ts`, `runtime/fabric/tests/execution-lifecycle.test.ts`, `schemas/README.md`, `P/fabric_hosts.py`, `P/instance_installation.py`, `P/model-route`, `P/model_route_catalog.py`, `P/utility.py`, `O/cf_dispatch.py`, `O/dispatch_preflight.py`, `O/dispatch_run.py`, `O/fabric_records.py`, `O/provider_exec.py`, `O/provider_output.py`, `tests/golden/cases/route/route-case-01.json`, `tests/golden/cases/route/route-case-02.json`, `tests/golden/cases/route/route-resolve.json`, `tests/golden/cases/routes/routes-case-01.json`, `tests/golden/cases/routes/routes-case-02.json`, `tests/test_actual_effort.py`, `tests/test_cf_dispatch.py`, `tests/test_dispatch_run.py`, `tests/test_fabric_hosts.py`, `tests/test_fabric_records.py`, `tests/test_harness_profile.py`, `tests/test_instance_installation.py`, `tests/test_model_route.py`, `tests/test_model_route_pools.py`, `tests/test_provider_exec.py`, `tests/test_split_root_contract.py`, `tests/test_task_classifier.py`, `C/routing/src/preferences.rs`, `C/routing/src/render.rs`.

### 15 — Telemetry ledger and ingest

**Owner:** Workshop; crates `telemetry, cli`. **Budget:** 2 lane-days. **Prerequisites:** 02,04,11,14; owner APIs A1,A2,A3,A4,A7,A9,A10.

**Commands, entries and tools:** `telemetry` ingest/status plus context_usage.py direct; no MCP.

**Existing Rust:** telemetry::{ingest,costs} and lib typed sinks: reuse parsers/reducers; implement ledger owner and safe readers rather than Python SQLite facsimile.

**Contract / judges:** J1 telemetry; J2 four telemetry row schemas; SQLite logical schema, cursor/watermark and dedup contracts. **Missing cases / blockers:** Append/truncate/rewrite files, no-follow races, duplicate events/OTLP privacy, OpenCode snapshot transaction and interrupted ingest; add context usage entry cases.

**Graph units:** `P/telemetry.py#{interface,unit-01…04}`; `P/telemetry_costs.py#{unit-01}`; `P/telemetry_ingest.py#{unit-01…02}`; `O/context_usage.py#{unit-01…02}`.

**Delete source (2,130 lines):** `P/telemetry.py`, `P/telemetry_costs.py`, `P/telemetry_ingest.py`, `O/context_usage.py`.

**Retire tests (3,543 lines, subject to the mixed-test rule):** `tests/test_context_usage.py`, `tests/test_otlp_collector.py`, `tests/test_telemetry.py`.

**Caller/reference closure to switch:** `docs/runbooks/telemetry.md`, `docs/specs/harness/profiles.md`, `docs/specs/harness/replay-bench.md`, `docs/specs/harness/rust-supervisor.md`, `schemas/README.md`, `P/fabric_hosts.py`, `P/provenant-telemetry`, `P/provider_quota.py`, `P/utility.py`, `O/dispatch_run.py`, `O/fabric_records.py`, `O/provider_exec.py`, `O/provider_observer.py`, `tests/golden/cases/retro/retro-case-01.json`, `tests/golden/cases/retro/retro-case-02.json`, `tests/golden/cases/stats/stats-case-01.json`, `tests/golden/cases/stats/stats-case-02.json`, `tests/golden/cases/telemetry/telemetry-case-01.json`, `tests/golden/cases/telemetry/telemetry-case-02.json`, `tests/test_cf_dispatch.py`, `tests/test_context_usage.py`, `tests/test_dispatch_run.py`, `tests/test_host_identity.py`, `tests/test_provider_quota.py`, `tests/test_record_schemas.py`, `tests/test_telemetry.py`, `C/core/src/py/datetime.rs`.

### 16 — Telemetry summaries and host snapshots

**Owner:** Workshop; crates `telemetry, cli`. **Budget:** 1.5 lane-days. **Prerequisites:** 15; owner APIs A1,A2,A3,A10.

**Commands, entries and tools:** `stats`, `telemetry stats`, quota-menubar fixture maker; host summary transport later 36.

**Existing Rust:** telemetry::{report,hosts}: reuse renderer and snapshot validator; finish ledger queries.

**Contract / judges:** J1 stats/telemetry; J2 telemetry/quota series, peer summary shape and exact missingness/cost rounding. **Missing cases / blockers:** Add seeded-ledger success, nearest-rank percentiles, nulls, snapshot limits and stale peer reads; quota menubar consumes same schema.

**Graph units:** `P/telemetry_hosts.py#{interface,unit-01}`; `P/telemetry_report.py#{unit-01…02}`.

**Delete source (706 lines):** `apps/quota-menubar/Fixtures/make_fixtures.py`, `P/telemetry_hosts.py`, `P/telemetry_report.py`.

**Retire tests (284 lines, subject to the mixed-test rule):** `tests/test_actual_effort.py`.

**Caller/reference closure to switch:** `apps/quota-menubar/README.md`, `P/telemetry.py`, `tests/test_telemetry.py`, `C/quota/src/tests/oracle.py`.

### 17 — Confinement and custody policy

**Owner:** Workshop (confinement/cli); Laptop (workflow); crates `confinement, workflow, cli`. **Budget:** 2 lane-days. **Prerequisites:** 02,05,08; owner APIs A1,A2,A3,A4,A5,A8.

**Commands, entries and tools:** provider_confinement.py, lane_protection.py, instruction_quarantine.py, output_custody.py, agy_home.py and secret_scan.py direct entries → Rust confinement commands. No MCP.

**Existing Rust:** confinement::{agy_home,instruction_quarantine,lane_protection,output_custody,trusted_git}; workflow::custody: reuse SafeFs-based policy, remove Python object emulation.

**Contract / judges:** J1 direct entry refusals; J2 grants/quarantine/receipts; Linux/macOS symlink, authority and release fencing conformance. **Missing cases / blockers:** Safe live-home overlay, failed restoration, reviewed-tip changed, wrong writer, output no-replace and secret redaction; never weaken permission boundaries.

**Graph units:** `skills/_shared/custody.py#{unit-01}`; `O/agy_home.py#{interface,unit-01}`; `O/instruction_quarantine.py#{interface,unit-01}`; `O/lane_protection.py#{interface,unit-01}`; `O/output_custody.py#{unit-01}`; `O/provider_confinement.py#{interface,unit-01…04}`; `O/secret_scan.py#{unit-01}`; `O/trusted_git.py#{unit-01}`.

**Delete source (2,643 lines):** `skills/_shared/custody.py`, `O/agy_home.py`, `O/instruction_quarantine.py`, `O/lane_protection.py`, `O/output_custody.py`, `O/provider_confinement.py`, `O/secret_scan.py`, `O/trusted_git.py`.

**Retire tests (718 lines, subject to the mixed-test rule):** `tests/test_cf_dispatch_git_custody.py`, `tests/test_issue_697_custody.py`.

**Caller/reference closure to switch:** `docs/specs/fabric-v2.md`, `docs/specs/harness/rust-supervisor-tests.md`, `F/cli.ts`, `runtime/fabric/tests/fabric.test.ts`, `P/fabric_git.py`, `P/worktree.py`, `skills/_shared/review_plan.py`, `skills/orchestrate/SKILL.md`, `O/batch_run.py`, `O/cf_dispatch.py`, `O/dispatch_preflight.py`, `O/dispatch_run.py`, `O/fabric_records.py`, `O/layout.py`, `O/log_reducer.py`, `O/provider_exec.py`, `O/run_controls.py`, `O/run_dir_finalize.py`, `O/run_lineage.py`, `O/run_records.py`, `O/stall_watchdog.py`, `tests/support/confinement.py`, `tests/test_ci_repository_assurance_policy.py`, `tests/test_dispatch_run.py`, `tests/test_fabric_hosts.py`, `tests/test_issue_697_custody.py`, `tests/test_provider_exec.py`, `tests/test_worktree_policy.py`.

### 18 — Execution planning and preflight

**Owner:** Workshop (providers/run/cli); Laptop (workflow); crates `providers, run, workflow, cli`. **Budget:** 1.5 lane-days. **Prerequisites:** 14,17; owner APIs A1,A2,A3,A5,A8,A9,A11.

**Commands, entries and tools:** provider_exec.py plan boundary and dispatch_preflight.py; typed owner calls used by 20. No MCP.

**Existing Rust:** providers::{exec_contract,dispatch_contract}; workflow::dispatch_preflight: reuse; caller supplies host facts via confinement, never providers→run/supervisor.

**Contract / judges:** J1 plan/direct preflight, J2 launch/permission/adapter records; exact argv/env/config application. **Missing cases / blockers:** Complete execution/rejection response shapes, unknown metadata, applied grants, prelaunch cancellation and provider plan failure.

**Graph units:** `O/dispatch_preflight.py#{interface,unit-01…02}`.

**Delete source (584 lines):** `O/dispatch_preflight.py`.

**Retire tests (158 lines, subject to the mixed-test rule):** `tests/test_exec_layout.py`.

**Caller/reference closure to switch:** `P/fabric_lanes.py`, `O/batch_run.py`, `O/dispatch_run.py`, `tests/test_provider_exec.py`, `C/workflow/tests/oracle.py`.

### 19 — Attempt records and observer boundary

**Owner:** Workshop; crates `records, supervisor, run, cli`. **Budget:** 2 lane-days. **Prerequisites:** 02,12,18; owner APIs A1,A2,A3,A8,A11.

**Commands, entries and tools:** provider_observer.py, lane_launch.py, lane_exit.py direct boundaries and `lane`; record publication entry points. No MCP.

**Existing Rust:** records::{attempt_evidence,attempt_storage,fabric_records,layout}; workflow::{provider_observer,lane_launch,lane_exit}; supervisor accepted S2: reuse typed records and custody.

**Contract / judges:** J2 attempt/owner/adapter/sidecar/launch/exit/watchdog schemas; lane_conformance; byte hashes and atomic publication order. **Missing cases / blockers:** Observer death/ENOSPC/reserve retry, launch 64/75 protocol, natural exit vs late cancel, no second writer and invalid opaque sidecars.

**Graph units:** `O/attempt_evidence.py#{unit-01}`; `O/attempt_storage.py#{unit-01}`; `O/fabric_records.py#{interface,unit-01…02}`; `O/lane_exit.py#{interface,unit-01}`; `O/lane_launch.py#{interface,unit-01}`; `O/layout.py#{interface,unit-01}`; `O/process_info.py#{unit-01…02}`; `O/provider_observer.py#{interface,unit-01}`; `O/run_records.py#{unit-01…02}`; `O/stall_watchdog.py#{unit-01}`; `O/worker_liveness.py#{unit-01…02}`.

**Delete source (3,202 lines):** `O/attempt_evidence.py`, `O/attempt_storage.py`, `O/fabric_records.py`, `O/lane_exit.py`, `O/lane_launch.py`, `O/layout.py`, `O/process_info.py`, `O/provider_observer.py`, `O/run_records.py`, `O/stall_watchdog.py`, `O/worker_liveness.py`.

**Retire tests (1,948 lines, subject to the mixed-test rule):** `tests/test_attempt_evidence.py`, `tests/test_fabric_records.py`, `tests/test_lane_exit.py`, `tests/test_lane_launch.py`, `tests/test_process_info.py`, `tests/test_worker_liveness.py`.

**Caller/reference closure to switch:** `docs/specs/fabric-v2.md`, `docs/specs/harness/graph-runner.md`, `docs/specs/harness/operating-model.md`, `docs/specs/harness/rust-supervisor-tests.md`, `docs/specs/harness/rust-supervisor.md`, `F/identity.ts`, `schemas/README.md`, `P/clean.py`, `P/model_route.py`, `skills/orchestrate/references/worker-liveness.md`, `O/agy_home.py`, `O/batch_run.py`, `O/bin/ps`, `O/dispatch_preflight.py`, `O/dispatch_run.py`, `O/exec_routing.py`, `O/memory_admission.py`, `O/provider_confinement.py`, `O/provider_exec.py`, `O/run_controls.py`, `O/run_dir_finalize.py`, `O/run_dir_init.sh`, `O/run_lineage.py`, `tests/test_actual_effort.py`, `tests/test_attempt_evidence.py`, `tests/test_ci_repository_assurance_policy.py`, `tests/test_context_usage.py`, `tests/test_exec_layout.py`, `tests/test_fabric_records.py`, `tests/test_memory_admission.py`, `tests/test_process_info.py`, `tests/test_provider_exec.py`, `tests/test_record_schemas.py`, `tests/test_worker_liveness.py`, `C/records/README.md`, `C/records/src/attempt_evidence/tests.rs`, `C/records/src/attempt_storage/tests.rs`, `C/records/src/fabric_records.rs`, `C/records/src/fabric_records/tests.rs`, `C/records/src/layout.rs`, `C/supervisor/README.md`, `C/supervisor/src/worker_liveness.rs`, `C/supervisor/src/worker_liveness/edge.rs`, `C/supervisor/src/worker_liveness/observe_tests.rs`, `C/supervisor/src/worker_liveness/tests.rs`, `C/workflow/tests/oracle.py`.

**Manual invocation/ownership evidence:** dispatch_run.py, provider_exec.py, batch_run.py, run_controls.py and Fabric run/process readers currently share these files. Switch those callers to Rust record/identity/launch APIs now; their whole scripts leave in 20–22/32. Existing workflow lane launch/exit/observer policy is Laptop-owned even in this Workshop-led custody slice. Relocate actual launch/observer execution into run/supervisor: workflow cannot import run or supervisor under R2; it retains only validation through its allowed dependencies.

### 20 — Dispatch owner

**Owner:** Workshop; crates `run, providers, cli`. **Budget:** 2 lane-days. **Prerequisites:** 15,18,19; owner APIs A1,A2,A3,A7,A8,A9,A10,A11.

**Commands, entries and tools:** `dispatch`; dispatch_run.py; no Fabric facade owner switch yet.

**Existing Rust:** run::{lease,lineage,detached} support only; dispatch orchestration largely new. Reuse accepted S2, never another launcher.

**Contract / judges:** J1 dispatch beyond help, J2 canonical/compat attempts, adapter/digest/owner records; full lane_conformance all adapters. **Missing cases / blockers:** Fake providers for success/failure/question/quota/retry, timeout/cancel before spawn and late, terminal publication and recovery.

**Graph units:** `O/dispatch_run.py#{unit-01…08}`.

**Delete source (2,483 lines):** `O/dispatch_run.py`.

**Retire tests (7,529 lines, subject to the mixed-test rule):** `tests/test_dispatch_run.py`, `tests/test_worker_completion_guidance.py`.

**Caller/reference closure to switch:** `config/harness-knobs.json`, `docs/adr/0013-thin-provenant-cli.md`, `docs/adr/0021-configured-workspace-dispatch-boundaries.md`, `docs/adr/0022-thin-fabric-mcp-execution-facade.md`, `docs/evals/skill-portfolio-2026/README.md`, `docs/specs/harness/graph-runner.md`, `docs/specs/harness/profiles.md`, `docs/specs/harness/rust-supervisor-tests.md`, `docs/specs/harness/rust-supervisor.md`, `runtime/fabric/README.md`, `runtime/fabric/live-sandbox-smoke.py`, `F/batch-execution.ts`, `F/execution.ts`, `F/process-identity.ts`, `F/resume.ts`, `runtime/fabric/tests/execution-lifecycle.test.ts`, `runtime/fabric/tests/execution-owner-fixture.mjs`, `runtime/fabric/tests/execution-pruning.test.ts`, `runtime/fabric/tests/graph-runner.test.ts`, `runtime/fabric/tests/lean-surface.test.ts`, `runtime/fabric/tests/lifecycle-owner-fixture.mjs`, `runtime/fabric/tests/named-sessions.test.ts`, `runtime/fabric/tests/process-reaping.test.ts`, `runtime/fabric/tests/surface.test.ts`, `runtime/fabric/tests/v2-owner-fixture.mjs`, `schemas/README.md`, `O/batch_run.py`, `O/cf_dispatch.py`, `O/run_controls.py`, `tests/conftest.py`, `tests/fixtures/current-routing-eval/dispatch_fixture.py`, `tests/golden/cases/batch/batch-case-01.json`, `tests/golden/cases/dispatch/dispatch-case-01.json`, `tests/golden/cases/help/help-case-01.json`, `tests/golden/cases/usage/usage-alias-01.json`, `tests/lane_conformance/README.md`, `tests/support/dispatch.py`, `tests/test_actual_effort.py`, `tests/test_batch_run.py`, `tests/test_ci_repository_assurance_policy.py`, `tests/test_context_resume.py`, `tests/test_dispatch_run.py`, `tests/test_fabric_hosts.py`, `tests/test_harness_profile.py`, `tests/test_install_skills.py`, `tests/test_issue_697_custody.py`, `tests/test_model_route_pools.py`, `tests/test_provider_exec.py`, `tests/test_record_schemas.py`, `tests/test_run_controls.py`, `tools/front-door/src/help/dispatch.txt`, `tools/front-door/src/help/general.txt`, `tools/front-door/src/main.rs`, `tools/front-door/tests/common/mod.rs`.

### 21 — Batch and run control

**Owner:** Workshop; crates `run, cli`. **Budget:** 2 lane-days. **Prerequisites:** 20; owner APIs A1,A2,A3,A11.

**Commands, entries and tools:** `batch`, `run show|inspect|prompt|result|diagnostic-log|retry|cancel|reduce`; batch_run.py/run_controls.py. No MCP facade switch.

**Existing Rust:** run::{lease,lineage,detached}: reuse; typed batch reducer/control API, no Python callback injection.

**Contract / judges:** J1 batch/run; J2 batch-summary, attempt and owner records; lane_conformance nested/batch custody. **Missing cases / blockers:** Mixed outcomes/task cancellation, resume-attempt/require-session, output limits, busy owners and dead launcher recovery.

**Graph units:** `O/batch_run.py#{unit-01…03}`; `O/lease.py#{unit-01}`; `O/run_controls.py#{unit-01…04}`; `O/run_lineage.py#{interface,unit-01…02}`.

**Delete source (2,912 lines):** `O/batch_run.py`, `O/lease.py`, `O/run_controls.py`, `O/run_lineage.py`.

**Retire tests (3,677 lines, subject to the mixed-test rule):** `runtime/fabric/tests/execution-owner-fixture.mjs`, `runtime/fabric/tests/lifecycle-owner-fixture.mjs`, `runtime/fabric/tests/v2-owner-fixture.mjs`, `tests/test_batch_run.py`, `tests/test_lease.py`, `tests/test_run_controls.py`.

**Caller/reference closure to switch:** `config/harness-knobs.json`, `docs/adr/0013-thin-provenant-cli.md`, `docs/adr/0021-configured-workspace-dispatch-boundaries.md`, `docs/adr/0022-thin-fabric-mcp-execution-facade.md`, `docs/evals/skill-portfolio-2026/README.md`, `docs/specs/harness/profiles.md`, `docs/specs/harness/rust-supervisor-tests.md`, `docs/specs/harness/rust-supervisor.md`, `runtime/fabric/README.md`, `F/batch-execution.ts`, `F/execution.ts`, `F/resume.ts`, `runtime/fabric/tests/execution-lifecycle.test.ts`, `runtime/fabric/tests/execution-owner-fixture.mjs`, `runtime/fabric/tests/execution-pruning.test.ts`, `runtime/fabric/tests/fixtures/graph-live-free.json`, `runtime/fabric/tests/graph-runner.test.ts`, `runtime/fabric/tests/lean-surface.test.ts`, `runtime/fabric/tests/lifecycle-owner-fixture.mjs`, `runtime/fabric/tests/named-sessions.test.ts`, `runtime/fabric/tests/surface.test.ts`, `runtime/fabric/tests/v2-owner-fixture.mjs`, `schemas/README.md`, `skills/autopilot/references/codex-operator.md`, `skills/autopilot/references/operating-loop.md`, `skills/orchestrate/references/paired-primary.md`, `O/dispatch_run.py`, `O/provider_exec.py`, `O/run_dir_finalize.py`, `tests/conftest.py`, `tests/golden/cases/batch/batch-case-01.json`, `tests/golden/cases/help/help-case-01.json`, `tests/golden/cases/run/run-case-01.json`, `tests/golden/cases/usage/usage-alias-01.json`, `tests/lane_conformance/support.py`, `tests/test_batch_run.py`, `tests/test_ci_repository_assurance_policy.py`, `tests/test_dispatch_run.py`, `tests/test_fabric_hosts.py`, `tests/test_harness_profile.py`, `tests/test_install_skills.py`, `tests/test_lease.py`, `tests/test_model_route_pools.py`, `tests/test_record_schemas.py`, `tests/test_run_controls.py`, `C/run/src/lease.rs`, `C/workflow/tests/oracle.py`, `tools/front-door/src/help.rs`, `tools/front-door/src/help/batch.txt`, `tools/front-door/src/help/general.txt`, `tools/front-door/src/help/run.txt`, `tools/front-door/src/main.rs`, `tools/front-door/tests/common/mod.rs`.

### 22 — Direct provider entry and detached workers

**Owner:** Workshop; crates `providers, supervisor, run, cli`. **Budget:** 2 lane-days. **Prerequisites:** 20,21; owner APIs A1,A2,A3,A8,A11.

**Commands, entries and tools:** cf_dispatch.py direct argv, provider_exec.py execute, run_worker_detached.py and bin/ps → Rust direct-dispatch/worker/process verbs. These are supported direct entries, not dead code.

**Existing Rust:** providers::{output,output_policy,failure_rules}; supervisor::{process_info,worker_liveness,ps}; run::detached: reuse/simplify; delete fake CF_DISPATCH seam after fake-provider gates.

**Contract / judges:** J1 direct-CLI liveness/argv/env; J2 output/provenance and receipts; lane_conformance continuation/custody. **Missing cases / blockers:** Recorded adapter streams, Unicode/non-UTF8 paths, malformed events, observed model provenance, OpenCode export and non-finite boundary disposition.

**Graph units:** `skills/_shared/bounded_process.py#{unit-01}`; `O/bin/ps#{unit-01}`; `O/cf_dispatch.py#{interface,unit-01…03}`; `O/provider_exec.py#{interface,unit-01…09}`; `O/provider_output.py#{interface,unit-01…02}`; `O/run_worker_detached.py#{unit-01}`.

**Delete source (4,949 lines):** `skills/_shared/bounded_process.py`, `O/bin/ps`, `O/cf_dispatch.py`, `O/provider_exec.py`, `O/provider_output.py`, `O/run_worker_detached.py`.

**Retire tests (12,616 lines, subject to the mixed-test rule):** `tests/test_bounded_process.py`, `tests/test_cf_dispatch.py`, `tests/test_provider_exec.py`, `tests/test_provider_observer.py`.

**Caller/reference closure to switch:** `MAINTAINING.md`, `config/adapter-compatibility.yaml`, `config/harness-knobs.json`, `docs/ARCHITECTURE.md`, `docs/adr/0021-configured-workspace-dispatch-boundaries.md`, `docs/adr/0023-codex-custom-providers-inline-config.md`, `docs/research/skill-portfolio-practices-2026.md`, `docs/specs/harness/profiles.md`, `docs/specs/harness/rust-supervisor-tests.md`, `docs/specs/harness/rust-supervisor.md`, `F/ps.mjs`, `runtime/fabric/tests/adapter-registry.test.ts`, `schemas/README.md`, `schemas/fixtures/launch-spec.invalid.json`, `schemas/fixtures/launch-spec.valid.json`, `P/fabric_git.py`, `P/fabric_hosts.py`, `P/fabric_lanes.py`, `P/utility.py`, `skills/_shared/roots.py`, `skills/orchestrate/evals/contract_cases.yaml`, `skills/orchestrate/references/direct-cli-fallback.md`, `skills/orchestrate/references/worker-liveness.md`, `O/agy_home.py`, `O/batch_run.py`, `O/capabilities.py`, `O/dispatch_preflight.py`, `O/dispatch_run.py`, `O/instruction_quarantine.py`, `O/lane_launch.py`, `O/lane_protection.py`, `O/provider_observer.py`, `O/run_lineage.py`, `tests/cf_dispatch_support.py`, `tests/conftest.py`, `tests/fixtures/fabric-v1/lane-a-handoff.md`, `tests/golden/cases/dispatch/dispatch-case-01.json`, `tests/lane_conformance/README.md`, `tests/support/proc.py`, `tests/test_actual_effort.py`, `tests/test_bounded_process.py`, `tests/test_cf_dispatch.py`, `tests/test_cf_dispatch_git_custody.py`, `tests/test_ci_repository_assurance_policy.py`, `tests/test_context_usage.py`, `tests/test_dispatch_run.py`, `tests/test_fabric_hosts.py`, `tests/test_lane_exit.py`, `tests/test_lane_launch.py`, `tests/test_provider_exec.py`, `tests/test_provider_observer.py`, `tests/test_provider_skill_boundaries.py`, `tests/test_record_schemas.py`, `tests/test_static_security_check.py`, `tests/test_worker_completion_guidance.py`, `C/providers/src/failure_rules.rs`, `C/run/src/detached_tests.rs`, `C/supervisor/README.md`, `C/supervisor/src/fabric_ps.rs`, `C/supervisor/src/ps.rs`, `workflows/codebase-polish.js`, `workflows/cross-verify.js`, `workflows/implement-run.js`.

### 23 — Utility operations

**Owner:** Workshop; crates `cli, run, records`. **Budget:** 1.5 lane-days. **Prerequisites:** 14,20,22; owner APIs A1,A2,A3,A9,A11,A12.

**Commands, entries and tools:** `digest`, `classify`, `extract`; utility.py; no MCP.

**Existing Rust:** cli has no complete utility owner: redesign over typed run and schema-validation API; keep deterministic classify shortcut.

**Contract / judges:** J1 utility stdout/error/exit/provider argv and confidentiality; extraction JSON Schema validation is externally visible. **Missing cases / blockers:** Add full fake-provider happy/error outputs and draft-schema cases. `scripts/utility-schema.cjs` invokes Ajv: no approved Rust schema crate exists; chair must settle implementation or ADR amendment before extract cut-over.

**Graph units:** `P/utility.py#{interface,unit-01}`.

**Delete source (257 lines):** `P/utility-schema.cjs`, `P/utility.py`.

**Retire tests (154 lines, subject to the mixed-test rule):** `tests/test_utility.py`.

**Caller/reference closure to switch:** `P/task_classifier.py`, `P/utility_output_hook.py`, `tests/golden/cases/classify/classify-case-01.json`, `tests/golden/cases/digest/digest-case-01.json`, `tests/golden/cases/extract/extract-case-01.json`, `tests/golden/test_golden_cli.py`, `tests/test_utility.py`, `tools/front-door/src/main.rs`.

### 24 — Finalization, memory and log reduction

**Owner:** Workshop; crates `run, cli`. **Budget:** 1.5 lane-days. **Prerequisites:** 19,21; owner APIs A1,A2,A3,A11.

**Commands, entries and tools:** run_dir_finalize.py, log_reducer.py and memory_admission.py direct modes → Rust run subcommands. No MCP.

**Existing Rust:** run::lineage plus records publication: reuse; finalization/log/memory policy new typed reducers.

**Contract / judges:** J1 direct entry argv/exits; J2 log-reduction and retained attempt/report bytes; no unsafe memory admission. **Missing cases / blockers:** Malformed manifests, archive/memory refuses, empty reductions, budget limits and failed no-replace finalization.

**Graph units:** `O/log_reducer.py#{unit-01}`; `O/memory_admission.py#{unit-01}`; `O/run_dir_finalize.py#{unit-01…03}`.

**Delete source (1,198 lines):** `O/log_reducer.py`, `O/memory_admission.py`, `O/run_dir_finalize.py`.

**Retire tests (2,002 lines, subject to the mixed-test rule):** `tests/test_log_reducer.py`, `tests/test_memory_admission.py`, `tests/test_run_dir_finalize.py`.

**Caller/reference closure to switch:** `docs/adr/0021-configured-workspace-dispatch-boundaries.md`, `docs/specs/harness/rust-supervisor.md`, `schemas/README.md`, `skills/implement/references/run-contract.md`, `skills/orchestrate/references/memory-scratchpad.md`, `O/dispatch_run.py`, `tests/test_batch_run.py`, `tests/test_ci_repository_assurance_policy.py`, `tests/test_dispatch_run.py`, `tests/test_install_skills.py`, `tests/test_issue_697_custody.py`, `tests/test_log_reducer.py`, `tests/test_memory_admission.py`, `tests/test_run_controls.py`, `tests/test_run_dir_finalize.py`, `workflows/implement-run.js`.

### 25 — Delivery validation policy

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, records, cli`. **Budget:** 2 lane-days. **Prerequisites:** 09,17,19; owner APIs A1,A2,A3,A5,A6,A13.

**Commands, entries and tools:** validate_delivery.py/software_delivery_validation.py and authority_mapping.py direct interfaces → Rust delivery validate/authority commands. No MCP.

**Existing Rust:** workflow::{authority_mapping,policy,custody}; records::delivery_run_shape: reuse; redesign validators around typed context instead of api dict.

**Contract / judges:** J1 direct validation pass/fail/diagnostics; J2 RUN/profile/authority/evidence contracts, hashes and retained Git binding. **Missing cases / blockers:** Missing schemas for delivery/authority records; all profile/risk/policy failure cases, source-order diagnostic decisions only where visible, secret/path denial and forged evidence.

**Graph units:** `skills/_shared/delivery_run_invariants.py#{unit-01}`; `D/authority_mapping.py#{unit-01…02}`; `D/delivery_policy_validation.py#{unit-01…02}`; `D/delivery_run_shape.py#{unit-01}`; `D/delivery_validation_artifacts.py#{unit-01}`; `D/delivery_validation_common.py#{unit-01}`; `D/delivery_validation_evidence.py#{unit-01}`; `D/delivery_validation_measures.py#{unit-01}`; `D/delivery_validation_reviews.py#{unit-01}`; `D/delivery_validation_security.py#{unit-01}`; `D/delivery_validation_shape.py#{unit-01}`; `D/software_delivery_validation.py#{unit-01}`; `D/validate_delivery.py#{unit-01}`.

**Delete source (2,739 lines):** `skills/_shared/delivery_run_invariants.py`, `D/authority_mapping.py`, `D/delivery_policy_validation.py`, `D/delivery_run_shape.py`, `D/delivery_validation_artifacts.py`, `D/delivery_validation_common.py`, `D/delivery_validation_evidence.py`, `D/delivery_validation_measures.py`, `D/delivery_validation_reviews.py`, `D/delivery_validation_security.py`, `D/delivery_validation_shape.py`, `D/software_delivery_validation.py`, `D/validate_delivery.py`.

**Retire tests (2,078 lines, subject to the mixed-test rule):** `tests/test_delivery_contract.py`, `tests/test_run_invariants_characterisation.py`.

**Caller/reference closure to switch:** `config/harness-knobs.json`, `docs/ARCHITECTURE.md`, `docs/adr/0005-lifecycle-kernel-extends-delivery.md`, `docs/runbooks/github-workflow.md`, `docs/specs/harness/profiles.md`, `P/clean.py`, `skills/deliver/SKILL.md`, `skills/deliver/references/contract.md`, `D/delivery_receipt.py`, `skills/implement/SKILL.md`, `skills/implement/references/run-contract.md`, `skills/implement/scripts/bind_merged_delivery.py`, `skills/implement/scripts/checkpoint_run.py`, `skills/session/scripts/cleanup_run_artifacts.py`, `tests/test_delivery_contract.py`, `tests/test_delivery_receipt_producer.py`, `tests/test_install_skills.py`, `tests/test_run_invariants_characterisation.py`, `tests/test_software_delivery_continuity.py`, `tests/test_telemetry.py`, `C/records/README.md`, `C/records/src/delivery_run_shape/tests.rs`, `C/workflow/src/authority_mapping.rs`, `C/workflow/tests/oracle.py`, `workflows/implement-run.js`.

### 26 — Receipt mutation and reference builders

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, cli`. **Budget:** 2 lane-days. **Prerequisites:** 25; owner APIs A1,A2,A3,A5,A6,A13.

**Commands, entries and tools:** delivery_receipt.py init/accept/bind/artifact/evidence/observation/review/checkpoint/repair/finding/show; reference_runs.py/reference_evaluation.py → Rust delivery commands. No MCP.

**Existing Rust:** workflow::policy plus safe publication; receipt transaction implementation new, simplify callback dispatch table.

**Contract / judges:** J1 all direct subcommands/stdin/exits; J2 receipt versions, digest binding and lock/publication; no fabricated acceptance. **Missing cases / blockers:** Conflicting concurrent receipt update, failed evidence process, reference stochastic anchors, high-stakes profile and rollback after write failure.

**Graph units:** `D/delivery_receipt.py#{unit-01…03}`; `D/delivery_receipt_commands.py#{unit-01…02}`; `D/delivery_receipt_process.py#{unit-01}`; `D/reference_evaluation.py#{unit-01…02}`; `D/reference_runs.py#{unit-01}`.

**Delete source (2,246 lines):** `D/delivery_receipt.py`, `D/delivery_receipt_commands.py`, `D/delivery_receipt_process.py`, `D/reference_evaluation.py`, `D/reference_runs.py`.

**Retire tests (1,291 lines, subject to the mixed-test rule):** `tests/test_delivery_receipt_producer.py`.

**Caller/reference closure to switch:** `docs/ARCHITECTURE.md`, `skills/_shared/delivery_run_invariants.py`, `skills/_shared/roots.py`, `skills/code-review/references/finding-contract.md`, `skills/deliver/SKILL.md`, `skills/deliver/references/contract.md`, `skills/implement/references/run-contract.md`, `tests/test_delivery_contract.py`, `tests/test_delivery_receipt_producer.py`, `tests/test_install_skills.py`, `tests/test_product_root_single_resolver.py`, `tests/test_run_artifact_cleanup.py`, `tests/test_run_invariants_characterisation.py`, `tests/test_software_delivery_continuity.py`, `workflows/implement-run.js`.

### 27 — Merged-delivery continuity and checkpoints

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, cli`. **Budget:** 1.5 lane-days. **Prerequisites:** 08,26; owner APIs A1,A2,A3,A5,A13.

**Commands, entries and tools:** bind_merged_delivery.py and checkpoint_run.py → Rust delivery bind-merged/checkpoint. No MCP.

**Existing Rust:** workflow::git_evidence_packet/custody reusable; merge/tree receipt bind remains new.

**Contract / judges:** J1 direct verbs and offline gh; J2 exact native commit/tree/evidence hash bindings, retained canonical RUN. **Missing cases / blockers:** Wrong reviewed head/tree, forbidden inherited Git env, replaced objects, failed PR/CI read, duplicate binding and immutable delivery anchors.

**Graph units:** `skills/implement/scripts/bind_merged_delivery.py#{unit-01}`; `skills/implement/scripts/checkpoint_run.py#{unit-01}`.

**Delete source (437 lines):** `skills/implement/scripts/bind_merged_delivery.py`, `skills/implement/scripts/checkpoint_run.py`.

**Retire tests (904 lines, subject to the mixed-test rule):** `skills/implement/tests/test_checkpoint_run.py`, `tests/test_implement_run_workflow.py`, `tests/test_software_delivery_continuity.py`.

**Caller/reference closure to switch:** `docs/runbooks/github-workflow.md`, `P/check-harness`, `skills/implement/references/run-contract.md`, `skills/implement/tests/test_checkpoint_run.py`, `tests/test_delivery_receipt_producer.py`, `tests/test_install_skills.py`, `tests/test_run_invariants_characterisation.py`, `tests/test_software_delivery_continuity.py`, `workflows/implement-run.js`.

### 28 — Worktrees

**Owner:** Workshop; crates `cli, confinement`. **Budget:** 2 lane-days. **Prerequisites:** 08,17,25; owner APIs A1,A2,A3,A5,A13.

**Commands, entries and tools:** `worktree create|list|check|context|verify|remove`; shell wrapper execs Rust; no MCP.

**Existing Rust:** cli::worktree and core::excludes/workspace_paths: reuse porcelain/pure policy; finish bounded Git and writer-lease effects.

**Contract / judges:** J1 real create/list/remove; retained reservation/claim bytes and deletion/refusal fencing. **Missing cases / blockers:** Dirty/unmerged worktree, race/rollback, raw path bytes, protected instruction gate and wrong common root; keep policy/cleanup tests until black-box replacements.

**Graph units:** `P/worktree.py#{interface,unit-01…03}`; `O/writer_lease.py#{unit-01}`.

**Delete source (1,227 lines):** `P/worktree.py`, `O/writer_lease.py`.

**Retire tests (1,018 lines, subject to the mixed-test rule):** `tests/test_worktree_policy.py`.

**Caller/reference closure to switch:** `.github/workflows/ci.yml`, `runtime/fabric/tests/fixtures/graph-live-free.json`, `P/check_target.py`, `P/clean.py`, `P/fabric_git.py`, `P/fabric_hosts.py`, `P/lib/stale_checkout.py`, `P/node-workspace-provision.py`, `P/worktree`, `O/batch_run.py`, `O/cf_dispatch.py`, `O/dispatch_preflight.py`, `O/dispatch_run.py`, `O/provider_exec.py`, `tests/golden/cases/worktree/worktree-case-01.json`, `tests/golden/cases/worktree/worktree-case-02.json`, `tests/test_cf_dispatch.py`, `tests/test_fabric_hosts.py`, `tests/test_live_sandbox_smoke.py`, `tests/test_node_workspace_provision.py`, `tests/test_stale.py`, `tests/test_worktree_policy.py`, `C/workflow/tests/oracle.py`.

### 29 — Retention cleanup and explicit reaping

**Owner:** Workshop; crates `cli, supervisor`. **Budget:** 2 lane-days. **Prerequisites:** 21,24,25,28; owner APIs A1,A2,A3,A5,A11,A13.

**Commands, entries and tools:** `clean`, `reap`; post-merge hook already calls clean. No MCP.

**Existing Rust:** cli::{clean,reap}: reuse planners/refusals; supervisor identity-authorized effects only, not inferred PID signals.

**Contract / judges:** J1 clean/reap; plan digest/apply/stale fences, allocated block counting and no-follow deletion; process conformance on both OSes. **Missing cases / blockers:** Changed plan/tree, unavailable GitHub state, live/pinned/unaccepted runs, PID reuse/ancestor refusal, late marker and partial removal failures.

**Graph units:** `P/clean.py#{interface,unit-01…03}`; `P/reap.py#{unit-01}`.

**Delete source (1,211 lines):** `P/clean.py`, `P/reap.py`.

**Retire tests (1,526 lines, subject to the mixed-test rule):** `tests/test_clean.py`.

**Caller/reference closure to switch:** `docs/specs/harness/rust-supervisor-tests.md`, `docs/specs/harness/rust-supervisor.md`, `P/worktree.py`, `tests/golden/cases/clean/clean-case-01.json`, `tests/golden/cases/clean/clean-case-02.json`, `tests/golden/cases/help/help-case-01.json`, `tests/golden/cases/usage/usage-alias-01.json`, `tests/golden/test_golden_cli.py`, `tests/test_clean.py`, `tests/test_lane_safety.py`, `C/cli/src/clean.rs`, `tools/front-door/src/help/clean.txt`, `tools/front-door/src/help/general.txt`, `tools/front-door/src/help/reap.txt`, `tools/front-door/src/main.rs`, `tools/front-door/tests/common/mod.rs`, `tools/front-door/tests/front_door.rs`.

### 30 — Delivery telemetry and retrospectives

**Owner:** Workshop; crates `telemetry, cli`. **Budget:** 1.5 lane-days. **Prerequisites:** 16,26,27; owner APIs A1,A2,A3,A5,A10,A13.

**Commands, entries and tools:** `retro`, telemetry changes/delivery/attention and public report modes. No MCP.

**Existing Rust:** telemetry::{changes,delivery,costs}: reuse reducers; implement SQL joins/GitHub range sync, simplify generic JSON copies.

**Contract / judges:** J1 retro and telemetry output; J2 ledger/receipt linking; exact attribution, missingness and safe cursor updates. **Missing cases / blockers:** Seed accepted/unaccepted receipts, stale sidecar invalidation, compacted context, partial peer summaries and failed range sync.

**Graph units:** `P/telemetry_changes.py#{unit-01…02}`; `P/telemetry_delivery.py#{unit-01}`.

**Delete source (682 lines):** `P/telemetry_changes.py`, `P/telemetry_delivery.py`.

**Retire tests (0 lines, subject to the mixed-test rule):** None.

**Caller/reference closure to switch:** `docs/runbooks/telemetry.md`, `P/telemetry.py`, `tests/test_telemetry.py`.

### 31 — Fabric store commands

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `fabric-store, fabric-runtime, cli`. **Budget:** 2 lane-days. **Prerequisites:** 01,02,06; owner APIs A1,A2,A3,A14.

**Commands, entries and tools:** `fabric send|inbox|ack|note|tasks|task|claim|done|activity|watch|work-claims|doctor`; identity/project/agent rows used by `whoami` and status; backend for fabric_send/inbox/acknowledge/task/note/activity/work_claim/landing_lease. MCP registration stays in TS until 37.

**Existing Rust:** fabric-store::{messages,tasks,sessions,landing,diagnostics}: reuse transactions/schema include; finish work claims/default open/diagnostic snapshots.

**Contract / judges:** J1 Fabric CLI; J2 SQLite schema.sql logical data/migrations; J3 stateful tool scenarios through existing edge. **Missing cases / blockers:** Concurrent expiry/claim races, WAL private snapshot retries, migration old↔new copies, rollback on logging failure, missing parent/UUID failures and busy timeout.

**Graph units:** `F/store.ts#{unit-01…04}`.

**Delete source (1,314 lines):** `F/store.ts`.

**Retire tests (639 lines, subject to the mixed-test rule):** `runtime/fabric/tests/claim-contention-worker.ts`, `runtime/fabric/tests/claim-worker.ts`, `runtime/fabric/tests/cold-start-worker.ts`, `runtime/fabric/tests/concurrency-worker.ts`, `runtime/fabric/tests/namespace-worker.ts`, `runtime/fabric/tests/task-claim-worker.ts`, `tests/test_runtime_architecture.py`.

**Caller/reference closure to switch:** `runtime/fabric/smoke.ts`, `F/cli.ts`, `F/execution.ts`, `F/graph/owner.ts`, `F/graph/store.ts`, `F/graph/surface.ts`, `F/landing-cli.ts`, `F/lane-wait.ts`, `F/peer-lanes.ts`, `F/run-reader.ts`, `F/server.ts`, `F/sessions.ts`, `F/surface.ts`, `runtime/fabric/tests/claim-contention-worker.ts`, `runtime/fabric/tests/claim-worker.ts`, `runtime/fabric/tests/cold-start-worker.ts`, `runtime/fabric/tests/concurrency-worker.ts`, `runtime/fabric/tests/execution-lifecycle.test.ts`, `runtime/fabric/tests/fabric.test.ts`, `runtime/fabric/tests/graph-runner.test.ts`, `runtime/fabric/tests/lane-wait.test.ts`, `runtime/fabric/tests/named-sessions.test.ts`, `runtime/fabric/tests/namespace-worker.ts`, `runtime/fabric/tests/project-identity.test.ts`, `runtime/fabric/tests/run-reader-events.test.ts`, `runtime/fabric/tests/task-claim-worker.ts`, `tests/test_runtime_architecture.py`, `C/fabric-store/src/oracle.rs`.

**Manual invocation/ownership evidence:** cli.ts/server.ts/sessions.ts and graph/store.ts import the TS store. Move each operation to Rust owner calls now; deleting store.ts cannot wait until only its imports are removed from one entry.

### 32 — Fabric identity and retained run reads

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `fabric-runtime, cli; supervisor API owned by Workshop`. **Budget:** 2 lane-days. **Prerequisites:** 19,21,31; owner APIs A1,A2,A3,A11,A14,A15.

**Commands, entries and tools:** `fabric whoami|status|output`, read side of `fabric dispatch list`, `lanes`, `events`; backend for fabric_whoami/status/runs/output/events. `runs` is an MCP tool, not a top-level Fabric CLI verb.

**Existing Rust:** fabric-runtime::{identity,run_registry,run_root,success_status,lane_watch}; supervisor::{fabric_ps,identity}: reuse typed identity; retained-status reducers/readers mostly new.

**Contract / judges:** J1 seeded events/lanes/Fabric; J2 owner/status schemas; J3 seeded reads and lane_conformance live/dead identity. **Missing cases / blockers:** Partial/corrupt terminal records, old layout, event cursor paging, raw bytes/output caps, dead launcher + live provider, recursive watch recovery and bounded wait cancellation.

**Graph units:** `F/identity.ts#{unit-01}`; `F/lane-wait.ts#{unit-01}`; `F/lane-watch.ts#{unit-01}`; `F/owner-output.ts#{unit-01}`; `F/process-identity.ts#{interface,unit-01…02}`; `F/ps.d.mts#{unit-01}`; `F/ps.mjs#{unit-01}`; `F/run-reader.ts#{unit-01}`; `F/run-registry.ts#{interface,unit-01}`; `F/run-root.ts#{unit-01}`; `F/run-status.ts#{unit-01…03}`; `F/seat-index.ts#{unit-01}`; `F/seat-lanes.ts#{unit-01}`; `F/success-status.ts#{unit-01}`; `integrate:5ef42cbe0051`.

**Delete source (2,777 lines):** `F/identity.ts`, `F/lane-wait.ts`, `F/lane-watch.ts`, `F/owner-output.ts`, `F/process-identity.ts`, `F/ps.d.mts`, `F/ps.mjs`, `F/run-reader.ts`, `F/run-registry.ts`, `F/run-root.ts`, `F/run-status.ts`, `F/seat-index.ts`, `F/seat-lanes.ts`, `F/success-status.ts`.

**Retire tests (2,382 lines, subject to the mixed-test rule):** `runtime/fabric/tests/fixture-run-root.test.ts`, `runtime/fabric/tests/helpers/temp-root.ts`, `runtime/fabric/tests/lane-reader.test.ts`, `runtime/fabric/tests/lane-wait.test.ts`, `runtime/fabric/tests/lane-watch.test.ts`, `runtime/fabric/tests/lanes-cli.test.ts`, `runtime/fabric/tests/process-reaping.test.ts`, `runtime/fabric/tests/project-identity.test.ts`, `runtime/fabric/tests/run-closure.test.ts`, `runtime/fabric/tests/run-reader-events.test.ts`, `runtime/fabric/tests/seat-index.test.ts`.

**Caller/reference closure to switch:** `runtime/fabric/smoke.ts`, `F/batch-execution.ts`, `F/cli.ts`, `F/execution-input.ts`, `F/execution.ts`, `F/graph/document.ts`, `F/graph/owner.ts`, `F/graph/store.ts`, `F/graph/surface.ts`, `F/landing-cli.ts`, `F/peer-lane-rows.ts`, `F/peer-lanes.ts`, `F/pools.ts`, `F/python-owner.ts`, `F/resume.ts`, `F/server.ts`, `F/sessions.ts`, `F/store.ts`, `F/surface.ts`, `runtime/fabric/tests/adapter-registry.test.ts`, `runtime/fabric/tests/claim-contention-worker.ts`, `runtime/fabric/tests/claim-worker.ts`, `runtime/fabric/tests/cold-start-worker.ts`, `runtime/fabric/tests/concurrency-worker.ts`, `runtime/fabric/tests/execution-lifecycle.test.ts`, `runtime/fabric/tests/execution-pruning.test.ts`, `runtime/fabric/tests/fabric.test.ts`, `runtime/fabric/tests/fixture-run-root.test.ts`, `runtime/fabric/tests/fixtures/graph-live-free.json`, `runtime/fabric/tests/graph-document.test.ts`, `runtime/fabric/tests/graph-gates.test.ts`, `runtime/fabric/tests/graph-runner.test.ts`, `runtime/fabric/tests/helpers/temp-root.ts`, `runtime/fabric/tests/host-liveness.test.ts`, `runtime/fabric/tests/lane-reader.test.ts`, `runtime/fabric/tests/lane-wait.test.ts`, `runtime/fabric/tests/lane-watch.test.ts`, `runtime/fabric/tests/lean-surface.test.ts`, `runtime/fabric/tests/lifecycle-owner-fixture.mjs`, `runtime/fabric/tests/namespace-worker.ts`, `runtime/fabric/tests/process-reaping.test.ts`, `runtime/fabric/tests/project-identity.test.ts`, `runtime/fabric/tests/run-closure.test.ts`, `runtime/fabric/tests/run-reader-events.test.ts`, `runtime/fabric/tests/seat-index.test.ts`, `runtime/fabric/tests/surface.test.ts`, `runtime/fabric/tests/task-claim-worker.ts`, `runtime/fabric/tests/temp-root.test.ts`, `C/fabric-runtime/src/identity/tests.rs`, `C/supervisor/README.md`, `C/supervisor/src/fabric_ps.rs`.

**Manual invocation/ownership evidence:** server.ts, cli.ts, execution.ts, batch-execution.ts, resume.ts, sessions.ts and seat/peer lane readers need the Rust read/identity API. Providers never get signalling authority through a status facade.

### 33 — Fabric TS execution facade

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `fabric-runtime, cli`. **Budget:** 2 lane-days. **Prerequisites:** 14,20,21,22,31,32; owner APIs A1,A2,A3,A9,A11,A14,A15.

**Commands, entries and tools:** `fabric dispatch|cancel|adapters`, backend for fabric_dispatch/tasks[]/cancel/adapters. Switch runtime TS execution calls to Rust run owner, never TS→Python owner.

**Existing Rust:** fabric-runtime::{execution,batch_execution,fabric_policy,json_subprocess}: reuse envelope/effect policy; implementation missing in PORT_STATUS, simplify bridge away.

**Contract / judges:** J1 Fabric dispatch validation/list; J2 launch/status/owner; J3 validation only currently; lane_conformance owns actual execution. **Missing cases / blockers:** Add J3 safe fake-provider dispatch/batch/cancel, queued cancellation, no unrecorded launch, bounded stdout and tool-cancel custody.

**Graph units:** `F/batch-execution.ts#{interface,unit-01}`; `F/catalogue.ts#{unit-01}`; `F/execution-input.ts#{unit-01…02}`; `F/execution.ts#{interface,unit-01…03}`; `F/json-subprocess.ts#{unit-01}`; `F/pools.ts#{unit-01}`; `F/python-owner.ts#{unit-01}`; `O/fabric_policy.py#{unit-01}`; `integrate:496bc5e66c6b`.

**Delete source (2,589 lines):** `F/batch-execution.ts`, `F/catalogue.ts`, `F/execution-input.ts`, `F/execution.ts`, `F/json-subprocess.ts`, `F/pools.ts`, `F/python-owner.ts`, `O/fabric_policy.py`.

**Retire tests (3,583 lines, subject to the mixed-test rule):** `runtime/fabric/tests/adapter-registry.test.ts`, `runtime/fabric/tests/batch-host-worker.ts`, `runtime/fabric/tests/catalogue.test.ts`, `runtime/fabric/tests/dispatch-host-worker.ts`, `runtime/fabric/tests/execution-lifecycle.test.ts`, `runtime/fabric/tests/execution-pruning.test.ts`, `runtime/fabric/tests/json-subprocess.test.ts`, `runtime/fabric/tests/nested-dispatch.test.ts`, `runtime/fabric/tests/temp-root.test.ts`.

**Caller/reference closure to switch:** `docs/runbooks/lint.md`, `docs/specs/harness/profiles.md`, `F/cli.ts`, `F/graph/document.ts`, `F/graph/owner.ts`, `F/graph/surface.ts`, `F/peer-lanes.ts`, `F/process-identity.ts`, `F/resume.ts`, `F/server.ts`, `F/sessions.ts`, `F/surface.ts`, `F/tool-schemas.ts`, `runtime/fabric/tests/adapter-registry.test.ts`, `runtime/fabric/tests/batch-host-worker.ts`, `runtime/fabric/tests/catalogue.test.ts`, `runtime/fabric/tests/dispatch-host-worker.ts`, `runtime/fabric/tests/execution-lifecycle.test.ts`, `runtime/fabric/tests/execution-pruning.test.ts`, `runtime/fabric/tests/graph-document.test.ts`, `runtime/fabric/tests/graph-runner.test.ts`, `runtime/fabric/tests/json-subprocess.test.ts`, `runtime/fabric/tests/lean-surface.test.ts`, `runtime/fabric/tests/nested-dispatch.test.ts`, `runtime/fabric/tests/project-identity.test.ts`, `runtime/fabric/tests/surface.test.ts`, `runtime/fabric/tests/temp-root.test.ts`, `schemas/README.md`, `P/provenant_lint.py`, `O/provider_exec.py`, `O/secret_scan.py`, `C/fabric-runtime/src/fabric_policy.rs`.

**Manual invocation/ownership evidence:** server.ts, cli.ts, resume.ts, graph owner and MCP tests must call the Rust facade. Do not introduce a second process owner inside fabric-runtime.

### 34 — Fabric named sessions and resume

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `fabric-runtime, fabric-store, cli`. **Budget:** 1.5 lane-days. **Prerequisites:** 33; owner APIs A1,A2,A3,A11,A14,A15.

**Commands, entries and tools:** `fabric session list|inspect|forget` and `--session`, `--fresh`, `--resume`, `--handoff` inputs on dispatch; fabric_session/fabric_dispatch. There is no standalone `fabric resume` verb.

**Existing Rust:** fabric-store::sessions reuse claims; runtime resume/session orchestration new; do not preserve Python-owner bridge.

**Contract / judges:** J1 CLI sessions; J3 stateful session inspection plus dispatch; J2 pointer and provider continuation identity. **Missing cases / blockers:** Competing turn/forget, no provider session, failed launch/session record, continuation_unsupported/fresh handoff, last clean turn and late owner death.

**Graph units:** `F/resume.ts#{unit-01}`; `F/sessions.ts#{unit-01}`.

**Delete source (592 lines):** `F/resume.ts`, `F/sessions.ts`.

**Retire tests (677 lines, subject to the mixed-test rule):** `runtime/fabric/tests/named-sessions.test.ts`.

**Caller/reference closure to switch:** `F/cli.ts`, `F/peer-lanes.ts`, `F/server.ts`, `runtime/fabric/tests/execution-lifecycle.test.ts`, `runtime/fabric/tests/surface.test.ts`.

### 35 — Bounded graphs and landing

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `fabric-runtime, fabric-store, cli`. **Budget:** 2 lane-days. **Prerequisites:** 28,31,33,34; owner APIs A1,A2,A3,A5,A11,A14,A15.

**Commands, entries and tools:** `graph`/`fabric graph`, `lease`/landing-* aliases; fabric_graph/fabric_landing_lease.

**Existing Rust:** fabric-runtime::gates and fabric-store::landing reuse pure receipt/lease policy; graph document/owner/store implementation largely new.

**Contract / judges:** J1 graph/lease and CLI aliases; J2 graph reservation/node receipt; J3 graph tool and concurrent-process graph/lease fixtures. **Missing cases / blockers:** Gate authorization/hash mismatch, queue bounds, owner death/node retry, writer reservation clash, cancellation handshake and landing generation/push fences.

**Graph units:** `F/graph/document.ts#{unit-01…02}`; `F/graph/gates.ts#{unit-01}`; `F/graph/owner.ts#{unit-01…02}`; `F/graph/store.ts#{unit-01…02}`; `F/graph/surface.ts#{unit-01}`; `F/landing-cli.ts#{unit-01}`.

**Delete source (1,840 lines):** `F/graph/document.ts`, `F/graph/gates.ts`, `F/graph/owner.ts`, `F/graph/store.ts`, `F/graph/surface.ts`, `F/landing-cli.ts`.

**Retire tests (1,054 lines, subject to the mixed-test rule):** `runtime/fabric/tests/graph-document.test.ts`, `runtime/fabric/tests/graph-gate-worker.ts`, `runtime/fabric/tests/graph-gates.test.ts`, `runtime/fabric/tests/graph-runner.test.ts`, `runtime/fabric/tests/landing-cli.test.ts`.

**Caller/reference closure to switch:** `F/cli.ts`, `F/server.ts`, `runtime/fabric/tests/graph-document.test.ts`, `runtime/fabric/tests/graph-gate-worker.ts`, `runtime/fabric/tests/graph-gates.test.ts`, `runtime/fabric/tests/graph-runner.test.ts`, `schemas/README.md`, `C/fabric-runtime/src/gates.rs`, `tools/front-door/tests/common/mod.rs`, `tools/front-door/tests/front_door.rs`.

**Manual invocation/ownership evidence:** MCP registration remains in server.ts until 37: it calls Rust graph/landing operations. The CLI lease alias maps acquire/release/verify/status/push to landing-* exactly as main.rs does.

### 36 — Fabric federation and Git landing transport

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `fabric-runtime, cli`. **Budget:** 2 lane-days. **Prerequisites:** 08,16,28,32,33,35; owner APIs A1,A2,A3,A5,A10,A11,A14,A15.

**Commands, entries and tools:** `hosts`, `peer`, remote lanes, fetch/landing-*; fabric_hosts/fabric_fetch and remote status/cancel/dispatch.

**Existing Rust:** fabric-runtime::hosts + telemetry::hosts pure policy reuse; scripts/fabric_git/hosts/lanes implementations new over safe host APIs.

**Contract / judges:** J1 hosts/peer/lanes/lease; J2 writer receipt/bundle and snapshots; J3 fake SSH fetch; no live host/provider tests. **Missing cases / blockers:** Operation-ID idempotency, unreachable/stale peer, exact host pinning, lost bundle/bad head, failed remote cancel/landing and no host fallback duplicate writer.

**Graph units:** `F/hosts.ts#{unit-01}`; `F/peer-lane-rows.ts#{unit-01}`; `F/peer-lanes.ts#{unit-01}`; `P/fabric_git.py#{interface,unit-01…04}`; `P/fabric_hosts.py#{interface,unit-01…03}`; `P/fabric_lanes.py#{interface,unit-01…04}`.

**Delete source (3,168 lines):** `F/hosts.ts`, `F/peer-lane-rows.ts`, `F/peer-lanes.ts`, `P/fabric_git.py`, `P/fabric_hosts.py`, `P/fabric_lanes.py`.

**Retire tests (4,197 lines, subject to the mixed-test rule):** `runtime/fabric/tests/host-liveness.test.ts`, `runtime/fabric/tests/hosts.test.ts`, `runtime/fabric/tests/peer-lane-rows.test.ts`, `tests/test_fabric_hosts.py`.

**Caller/reference closure to switch:** `.github/workflows/ci.yml`, `docs/runbooks/telemetry.md`, `docs/specs/harness/rust-supervisor.md`, `docs/specs/observability-console.md`, `F/batch-execution.ts`, `F/cli.ts`, `F/execution.ts`, `F/resume.ts`, `F/run-reader.ts`, `F/run-status.ts`, `F/server.ts`, `F/sessions.ts`, `runtime/fabric/tests/hosts.test.ts`, `runtime/fabric/tests/peer-lane-rows.test.ts`, `P/fabric-hosts`, `P/telemetry_hosts.py`, `O/dispatch_preflight.py`, `O/dispatch_run.py`, `O/provider_confinement.py`, `tests/conftest.py`, `tests/golden/cases/hosts/hosts-case-01.json`, `tests/golden/cases/hosts/hosts-case-02.json`, `tests/golden/cases/peer/peer-alias-01.json`, `tests/test_ci_repository_assurance_policy.py`, `tests/test_dispatch_run.py`, `tests/test_fabric_hosts.py`, `tests/test_host_identity.py`, `tests/test_pytest_sharding.py`.

### 37 — MCP server cut-over

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `fabric-mcp, cli`. **Budget:** 2 lane-days. **Prerequisites:** 31,32,33,34,35,36; owner APIs A1,A2,A3,A11,A14,A15,A16.

**Commands, entries and tools:** Default seat/no-argv MCP startup; all 20 registered fabric_* tools listed below, tools/list and schema export.

**Existing Rust:** fabric-mcp::lib is a scaffold: implement small stdio JSON-RPC transport, bounded worker threads and cancellation; no SDK/async runtime clone.

**Contract / judges:** J3 protocol.jsonl/core scenarios and J2 inputs/retained state; J1 no-argv startup/env. Preserve content blocks, structured replies/errors and framing. **Missing cases / blockers:** Malformed/oversize frames, parallel requests, cancel while bounded wait/tool runs, EOF shutdown/reap, unknown fields, detailed errors and transport backpressure; J3 current dispatch cases stop at validation.

**Graph units:** `F/export-tool-schemas.ts#{unit-01}`; `F/server.ts#{unit-01…02}`; `F/surface.ts#{unit-01}`; `F/tool-schemas.ts#{unit-01}`.

**Delete source (997 lines):** `F/export-tool-schemas.ts`, `F/server.ts`, `F/surface.ts`, `F/tool-schemas.ts`.

**Retire tests (1,406 lines, subject to the mixed-test rule):** `runtime/fabric/tests/lean-surface.test.ts`, `runtime/fabric/tests/surface.test.ts`, `runtime/fabric/tests/tool-budget.mjs`.

**Caller/reference closure to switch:** `docs/specs/fabric-v2.md`, `F/cli.ts`, `F/execution.ts`, `F/lane-wait.ts`, `runtime/fabric/tests/fabric.test.ts`, `runtime/fabric/tests/hosts.test.ts`, `runtime/fabric/tests/lanes-cli.test.ts`, `runtime/fabric/tests/lean-surface.test.ts`, `runtime/fabric/tests/named-sessions.test.ts`, `runtime/fabric/tests/project-identity.test.ts`, `runtime/fabric/tests/run-reader-events.test.ts`, `runtime/fabric/tests/surface.test.ts`, `tests/fixtures/fabric-v1/lane-d-tool-params.json`, `tests/test_fabric_hosts.py`.

**Manual invocation/ownership evidence:** All registered tools: fabric_whoami, fabric_send, fabric_inbox, fabric_runs, fabric_events, fabric_dispatch, fabric_status, fabric_work_claim, fabric_landing_lease, fabric_session, fabric_cancel, fabric_graph, fabric_fetch, fabric_output, fabric_acknowledge, fabric_task, fabric_note, fabric_activity, fabric_hosts, fabric_adapters. No fabric_batch resurrection.

### 38 — Fabric CLI and Node exit

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `cli, fabric-runtime; config API owned by Workshop`. **Budget:** 1.5 lane-days. **Prerequisites:** 37; owner APIs A1,A2,A3,A4,A16.

**Commands, entries and tools:** Remaining `fabric` CLI table, runtime/fabric/bin/{fabric,fabric-mcp}; configure-fabric-mcp.py now registers Rust seat directly.

**Existing Rust:** cli parser/rendering + config install identity; runtime owner already Rust; no second dispatcher in cli.

**Contract / judges:** J1 every route/alias/env; J3 registered startup; package-install/MCP smoke rewritten for Rust installed candidate. **Missing cases / blockers:** Runtime without Node/python on PATH; install/uninstall stale registration, signal forwarding, exact default seat startup, package wrapper cwd/env.

**Graph units:** `F/cli.ts#{unit-01…03}`; `P/configure-fabric-mcp.py#{unit-01…03}`; `P/node-workspace-provision.py#{unit-01}`; `P/retry-npm-install.py#{unit-01}`.

**Delete source (2,261 lines):** `runtime/fabric/mcp-smoke.mjs`, `runtime/fabric/package-install-smoke.mjs`, `runtime/fabric/smoke.ts`, `F/cli.ts`, `runtime/fabric/vitest.config.ts`, `P/configure-fabric-mcp.py`, `P/node-workspace-preflight.mjs`, `P/node-workspace-provision.py`, `P/retry-npm-install.py`.

**Retire tests (4,146 lines, subject to the mixed-test rule):** `runtime/fabric/tests/fabric.test.ts`, `tests/test_configure_fabric_mcp.py`, `tests/test_fabric_node_discovery.py`, `tests/test_node_workspace_preflight.py`, `tests/test_node_workspace_provision.py`, `tests/test_npm_attestation_negative.py`, `tests/test_retry_npm_install.py`.

**Caller/reference closure to switch:** `.github/actions/setup-node-workspace/action.yml`, `.github/pull_request_template.md`, `.github/workflows/ci.yml`, `MAINTAINING.md`, `docs/adr/0019-installed-file-class-ownership.md`, `docs/runbooks/fabric-mcp-registration.md`, `package.json`, `runtime/fabric/README.md`, `runtime/fabric/package.json`, `runtime/fabric/tests/fabric.test.ts`, `runtime/fabric/tests/hosts.test.ts`, `runtime/fabric/tests/landing-cli.test.ts`, `runtime/fabric/tests/lanes-cli.test.ts`, `runtime/fabric/tests/project-identity.test.ts`, `runtime/fabric/tests/surface.test.ts`, `P/gate`, `P/install-harness`, `P/worktree.py`, `tests/mcp_judge/README.md`, `tests/test_ci_repository_assurance_policy.py`, `tests/test_configure_fabric_mcp.py`, `tests/test_node_workspace_preflight.py`, `tests/test_node_workspace_provision.py`, `tests/test_retry_npm_install.py`, `tests/test_worktree_policy.py`, `C/cli/src/retry_install.rs`.

**Manual invocation/ownership evidence:** runtime/fabric/bin/fabric and bin/fabric-mcp are shell/package callers; main.rs routes seat/no-argv through the MCP registration. npm workspace removal waits until UI/workflow Node callers leave in 49.

### 39 — UI evidence text engine

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, cli`. **Budget:** 1.5 lane-days. **Prerequisites:** 03,01; owner APIs A1,A2.

**Commands, entries and tools:** detect.mjs text/file detection → proposed `provenant ui detect`; no MCP.

**Existing Rust:** No inert UI Rust; redesign rule registry, findings and text scanning as typed Rust data.

**Contract / judges:** J1 direct detector CLI argv/report/exit; findings and version/provenance files; regex semantics only at reported findings. **Missing cases / blockers:** No UI J1 cases: add deterministic source/Markdown/text patterns, stable ordering, severity/profile, malformed file and output schema.

**Graph units:** **Graph gap**: add command/entry units through derive_graph.py before assigning; no fabricated IDs.

**Delete source (2,759 lines):** `runtime/ui-evidence/detector/engines/regex/detect-text.mjs`, `runtime/ui-evidence/detector/findings.mjs`, `runtime/ui-evidence/detector/registry/antipatterns.mjs`, `runtime/ui-evidence/detector/rules/checks.mjs`, `runtime/ui-evidence/detector/shared/constants.mjs`.

**Retire tests (0 lines, subject to the mixed-test rule):** None.

**Caller/reference closure to switch:** `runtime/ui-evidence/build-browser-detector.mjs`, `runtime/ui-evidence/build-browser-detector.test.mjs`, `runtime/ui-evidence/detect.mjs`, `runtime/ui-evidence/detector/cli/main.mjs`, `runtime/ui-evidence/detector/detect-antipatterns-browser.js`, `runtime/ui-evidence/detector/detect-antipatterns.mjs`, `runtime/ui-evidence/detector/engines/browser/detect-url.mjs`, `runtime/ui-evidence/detector/engines/static-html/css-cascade.mjs`, `runtime/ui-evidence/detector/engines/static-html/detect-html.mjs`, `runtime/ui-evidence/provenance_components.yaml`, `tests/test_ui_ux_detector_cli.py`.

### 40 — UI static HTML and CSS detection

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, cli`. **Budget:** 2 lane-days. **Prerequisites:** 39; owner APIs A1,A2,A17.

**Commands, entries and tools:** UI detector static HTML/CSS entry mode; same proposed ui detect. No MCP.

**Existing Rust:** No inert port. Redesign parsers/cascade around actual fixtures; no unapproved HTML/CSS parser dependency.

**Contract / judges:** J1 direct HTML detection report and safety/path effects; provenance. **Missing cases / blockers:** Freeze cascade/specificity/style extraction and selector edge results; chair settles dependency-free parser scope before implementation, not a regex substitute for unobserved DOM behaviour.

**Graph units:** **Graph gap**: add command/entry units through derive_graph.py before assigning; no fabricated IDs.

**Delete source (1,486 lines):** `runtime/ui-evidence/detector/engines/static-html/css-cascade.mjs`, `runtime/ui-evidence/detector/engines/static-html/detect-html.mjs`, `runtime/ui-evidence/detector/node/file-system.mjs`, `runtime/ui-evidence/detector/shared/color.mjs`.

**Retire tests (0 lines, subject to the mixed-test rule):** None.

**Caller/reference closure to switch:** `runtime/ui-evidence/build-browser-detector.mjs`, `runtime/ui-evidence/build-browser-detector.test.mjs`, `runtime/ui-evidence/detector/cli/main.mjs`, `runtime/ui-evidence/detector/detect-antipatterns-browser.js`, `runtime/ui-evidence/detector/detect-antipatterns.mjs`, `runtime/ui-evidence/detector/rules/checks.mjs`, `runtime/ui-evidence/provenance_components.yaml`, `tests/test_ui_ux_detector_cli.py`.

### 41 — UI detector browser and screenshot transport

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, cli; supervisor API owned by Workshop`. **Budget:** 2 lane-days. **Prerequisites:** 02,40; owner APIs A1,A2,A3,A17.

**Commands, entries and tools:** URL/browser/screenshot detector modes and profiler; same direct detect entry. No MCP.

**Existing Rust:** No inert port. Safe browser host boundary; Rust owns rule evaluation and findings.

**Contract / judges:** J1 URL mode/timeout/exits; local fake browser wire/DOM/screenshot fixtures; filesystem confinement. **Missing cases / blockers:** Browser-executed DOM JS and screenshot tool boundary unverified: chair must settle Rust/browser design and dependency scope; no silent removal of URL mode.

**Graph units:** **Graph gap**: add command/entry units through derive_graph.py before assigning; no fabricated IDs.

**Delete source (658 lines):** `runtime/ui-evidence/detector/engines/browser/detect-url.mjs`, `runtime/ui-evidence/detector/engines/visual/screenshot-contrast.mjs`, `runtime/ui-evidence/detector/profile/profiler.mjs`, `runtime/ui-evidence/detector/shared/page.mjs`.

**Retire tests (0 lines, subject to the mixed-test rule):** None.

**Caller/reference closure to switch:** `runtime/ui-evidence/detector/cli/main.mjs`, `runtime/ui-evidence/detector/detect-antipatterns.mjs`, `runtime/ui-evidence/detector/engines/regex/detect-text.mjs`, `runtime/ui-evidence/detector/engines/static-html/css-cascade.mjs`, `runtime/ui-evidence/detector/engines/static-html/detect-html.mjs`, `runtime/ui-evidence/provenance_components.yaml`, `tests/test_ui_ux_detector_cli.py`.

### 42 — UI detector entry and generated bundle

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, cli`. **Budget:** 1.5 lane-days. **Prerequisites:** 39,40,41; owner APIs A1,A2,A3,A17.

**Commands, entries and tools:** runtime and skill detect wrappers, browser detector injection; proposed ui detect complete. No MCP.

**Existing Rust:** No inert port. One Rust entry owns all modes; generated browser bundle removed only after replacement reaches real browser callers.

**Contract / judges:** J1 all direct wrapper paths and source/URL modes; browser asset/provenance independent tests. **Missing cases / blockers:** Chair decision required for browser injected index and generated JS: browser cannot exec native Rust. Do not claim this bundle dead; preserve functionality through accepted Rust/Wasm or approved boundary redesign before deleting.

**Graph units:** **Graph gap**: add command/entry units through derive_graph.py before assigning; no fabricated IDs.

**Delete source (6,321 lines):** `runtime/ui-evidence/build-browser-detector.mjs`, `runtime/ui-evidence/detect.mjs`, `runtime/ui-evidence/detector/browser/injected/index.mjs`, `runtime/ui-evidence/detector/cli/main.mjs`, `runtime/ui-evidence/detector/detect-antipatterns-browser.js`, `runtime/ui-evidence/detector/detect-antipatterns.mjs`, `skills/ui-ux-design/scripts/detect.mjs`, `skills/ui-ux-design/scripts/ui-evidence-paths.mjs`.

**Retire tests (708 lines, subject to the mixed-test rule):** `runtime/ui-evidence/build-browser-detector.test.mjs`, `tests/test_ui_ux_detector_cli.py`.

**Caller/reference closure to switch:** `THIRD_PARTY_NOTICES.md`, `docs/ARCHITECTURE.md`, `runtime/ui-evidence/README.md`, `runtime/ui-evidence/build-browser-detector.test.mjs`, `runtime/ui-evidence/provenance_components.yaml`, `skills/ui-ux-design/evals/provenance_components.yaml`, `skills/ui-ux-design/references/review.md`, `skills/ui-ux-design/references/visual-qa.md`, `tests/test_ui_ux_detector_cli.py`, `tests/test_ui_ux_provenance_contract.py`.

**Manual invocation/ownership evidence:** live-server.mjs:53 and detector/engines/browser/detect-url.mjs:130 load detect-antipatterns-browser.js. It is a generated runtime bundle, not a test-only artifact.

### 43 — UI live source and context

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, confinement, cli`. **Budget:** 1.5 lane-days. **Prerequisites:** 17,40; owner APIs A1,A2,A3,A17.

**Commands, entries and tools:** live source/context/CSP/JSX/containment helpers and detect-csp entry → Rust ui live/csp modes. No MCP.

**Existing Rust:** No inert UI port; use confinement/SafeFs for safe source operations and typed context.

**Contract / judges:** J1 direct CSP and context output; safe path/journal files; source bounds/security cases survive as black-box fixtures. **Missing cases / blockers:** Symlink swaps, sibling prefixes, injected path traversal, malformed JSX, stale/generated files and external context root.

**Graph units:** **Graph gap**: add command/entry units through derive_graph.py before assigning; no fabricated IDs.

**Delete source (2,565 lines):** `runtime/ui-live/contained-source.mjs`, `runtime/ui-live/design-parser.mjs`, `runtime/ui-live/detect-csp.mjs`, `runtime/ui-live/impeccable-paths.mjs`, `runtime/ui-live/is-generated.mjs`, `runtime/ui-live/jsx-tag-scanner.mjs`, `runtime/ui-live/load-context.mjs`, `skills/ui-ux-design/scripts/detect-csp.mjs`.

**Retire tests (2,433 lines, subject to the mixed-test rule):** `tests/test_frontend_context_loader.py`, `tests/test_frontend_source_security.py`, `tests/test_pytest_sharding.py`.

**Caller/reference closure to switch:** `runtime/ui-live/live-accept.mjs`, `runtime/ui-live/live-complete.mjs`, `runtime/ui-live/live-inject.mjs`, `runtime/ui-live/live-poll.mjs`, `runtime/ui-live/live-server.mjs`, `runtime/ui-live/live-session-store.mjs`, `runtime/ui-live/live-status.mjs`, `runtime/ui-live/live-wrap.mjs`, `runtime/ui-live/live.mjs`, `runtime/ui-live/provenance_components.yaml`, `skills/ui-ux-design/evals/provenance_components.yaml`, `tests/test_frontend_context_loader.py`, `tests/test_frontend_source_security.py`, `tests/test_pytest_sharding.py`, `tests/test_ui_ux_provenance_contract.py`.

### 44 — UI live journal and recovery

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, cli`. **Budget:** 2 lane-days. **Prerequisites:** 43; owner APIs A1,A2,A3,A17.

**Commands, entries and tools:** live-status, live-resume, live-complete, live-completion and live-session-store; direct entries → Rust ui live modes. No MCP.

**Existing Rust:** No inert port; typed bounded state machine with atomic journal owner, not shared JS maps.

**Contract / judges:** J1 direct state transitions/exits; retained session journal/read-back/lease and offline recovery. **Missing cases / blockers:** Reconnection terminal replay, expired history, competing leases, carbonize recovery, token redaction and crash persistence; convert relevant frontend_live_server_security cases before deleting Python assertions.

**Graph units:** **Graph gap**: add command/entry units through derive_graph.py before assigning; no fabricated IDs.

**Delete source (732 lines):** `runtime/ui-live/live-complete.mjs`, `runtime/ui-live/live-completion.mjs`, `runtime/ui-live/live-resume.mjs`, `runtime/ui-live/live-session-store.mjs`, `runtime/ui-live/live-status.mjs`.

**Retire tests (0 lines, subject to the mixed-test rule):** None.

**Caller/reference closure to switch:** `runtime/ui-live/live-poll.mjs`, `runtime/ui-live/live-server.mjs`, `runtime/ui-live/provenance_components.yaml`, `skills/ui-ux-design/evals/provenance_components.yaml`, `skills/ui-ux-design/references/live.md`, `skills/ui-ux-design/scripts/live-complete.mjs`, `skills/ui-ux-design/scripts/live-resume.mjs`, `skills/ui-ux-design/scripts/live-status.mjs`, `tests/test_frontend_live_server_security.py`, `tests/test_frontend_source_security.py`, `tests/ui_ux_live_test_support.py`.

### 45 — UI live process and HTTP owner

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, cli; supervisor API owned by Workshop`. **Budget:** 2 lane-days. **Prerequisites:** 02,41,44; owner APIs A1,A2,A3,A17.

**Commands, entries and tools:** live-server, live-process, startup/stop/poll/reply endpoints → Rust ui live server. No MCP.

**Existing Rust:** No inert port; std TCP/threads bounded transport, supervisor owns spawned browser/provider processes. HTTP/JSON framing reviewed before accepting in-house implementation.

**Contract / judges:** J1 direct server/poll contracts and fake browser HTTP/SSE fixtures; durable leases and no-follow files. **Missing cases / blockers:** Authenticated/null origin/CORS, oversize bodies, queue/session/SSE caps, token custody, startup/port races, disconnect and descendant cleanup.

**Graph units:** **Graph gap**: add command/entry units through derive_graph.py before assigning; no fabricated IDs.

**Delete source (2,126 lines):** `runtime/ui-live/live-poll.mjs`, `runtime/ui-live/live-process.mjs`, `runtime/ui-live/live-server-startup.mjs`, `runtime/ui-live/live-server.mjs`.

**Retire tests (673 lines, subject to the mixed-test rule):** `runtime/ui-live/tests/live-server-startup.test.mjs`, `tests/test_frontend_live_process_security.py`.

**Caller/reference closure to switch:** `runtime/ui-live/live-status.mjs`, `runtime/ui-live/live.mjs`, `runtime/ui-live/load-context.mjs`, `runtime/ui-live/provenance_components.yaml`, `runtime/ui-live/tests/live-server-startup.test.mjs`, `skills/ui-ux-design/evals/provenance_components.yaml`, `skills/ui-ux-design/references/live.md`, `skills/ui-ux-design/scripts/live-poll.mjs`, `skills/ui-ux-design/scripts/live-server.mjs`, `tests/test_frontend_source_security.py`, `tests/test_ui_ux_provenance_contract.py`, `tests/ui_ux_live_test_support.py`.

### 46 — UI browser interaction and live entry

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, cli`. **Budget:** 2 lane-days. **Prerequisites:** 42,45; owner APIs A1,A2,A3,A17.

**Commands, entries and tools:** live/live-wrap/live-inject/live-accept and installed skill wrappers; browser overlay, session UI, screenshots. No MCP.

**Existing Rust:** No inert port. Rust owns durable policy; browser interaction design is blocked on A17, not a licence to drop the feature.

**Contract / judges:** J1 direct installed live wrappers; independent real-browser interaction/provenance plus HTTP/state contracts. **Missing cases / blockers:** Chair acceptance needed for removing live-browser.js/session.js/modern-screenshot: they are loaded by live-server, not CI-only assets. Browser feature preservation and Rust-only source delivery remain unverified until A17 settled.

**Graph units:** **Graph gap**: add command/entry units through derive_graph.py before assigning; no fabricated IDs.

**Delete source (8,288 lines):** `runtime/ui-live/live-accept.mjs`, `runtime/ui-live/live-browser-session.js`, `runtime/ui-live/live-browser.js`, `runtime/ui-live/live-inject.mjs`, `runtime/ui-live/live-wrap.mjs`, `runtime/ui-live/live.mjs`, `runtime/ui-live/modern-screenshot.umd.js`, `skills/ui-ux-design/scripts/live-accept.mjs`, `skills/ui-ux-design/scripts/live-complete.mjs`, `skills/ui-ux-design/scripts/live-inject.mjs`, `skills/ui-ux-design/scripts/live-poll.mjs`, `skills/ui-ux-design/scripts/live-resume.mjs`, `skills/ui-ux-design/scripts/live-server.mjs`, `skills/ui-ux-design/scripts/live-status.mjs`, `skills/ui-ux-design/scripts/live-wrap.mjs`, `skills/ui-ux-design/scripts/live.mjs`, `skills/ui-ux-design/scripts/ui-live-paths.mjs`, `skills/ui-ux-design/scripts/ui-runtime-paths.mjs`.

**Retire tests (1,817 lines, subject to the mixed-test rule):** `tests/test_frontend_live_server_security.py`, `tests/ui_ux_live_test_support.py`.

**Caller/reference closure to switch:** `THIRD_PARTY_NOTICES.md`, `runtime/ui-live/detect-csp.mjs`, `runtime/ui-live/live-complete.mjs`, `runtime/ui-live/live-completion.mjs`, `runtime/ui-live/live-poll.mjs`, `runtime/ui-live/live-resume.mjs`, `runtime/ui-live/live-server.mjs`, `runtime/ui-live/live-status.mjs`, `runtime/ui-live/load-context.mjs`, `runtime/ui-live/provenance_components.yaml`, `runtime/ui-live/tests/live-server-startup.test.mjs`, `skills/ui-ux-design/evals/provenance_components.yaml`, `skills/ui-ux-design/references/live.md`, `skills/ui-ux-design/scripts/detect-csp.mjs`, `skills/ui-ux-design/scripts/ui-evidence-paths.mjs`, `tests/test_frontend_live_server_security.py`, `tests/test_frontend_source_security.py`, `tests/test_ui_ux_detector_cli.py`, `tests/test_ui_ux_provenance_contract.py`, `tests/ui_ux_live_test_support.py`.

**Manual invocation/ownership evidence:** live-server.mjs:373–374 serves live-browser-session.js and live-browser.js; :747–748 serves modern-screenshot.umd.js. Do not delete assets based on absent ESM imports.

### 47 — Installed workflow accelerators and tracker hook

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, cli`. **Budget:** 2 lane-days. **Prerequisites:** 09,26,27,28,33; owner APIs A1,A2,A3,A5,A6,A11,A18.

**Commands, entries and tools:** workflows/{implement-run,codebase-polish,cross-verify}.js; tracker steward and Claude tracker-route template → Rust workflow/tracker front-door calls. No new MCP.

**Existing Rust:** workflow::review_panel/portable_workflow/custody reuse; workflow orchestration and tracker queue typed new.

**Contract / judges:** J1 direct workflow/tracker invocations; retained workflow/queue records; harness workflow loader/install contract. **Missing cases / blockers:** Installed workflow host currently imports JS exports: chair must settle loader→Rust invocation before deletion. Preserve meta/run and patch/apply/repair semantics through black-box host fixtures. Herdr checker retained until this caller audit settles it.

**Graph units:** `O/check_herdr_cli.py#{unit-01}`; `skills/setup-repo/templates/claude-tracker-hook.py#{unit-01}`; `skills/tracker/scripts/steward.py#{unit-01}`.

**Delete source (2,310 lines):** `O/check_herdr_cli.py`, `skills/setup-repo/templates/claude-tracker-hook.py`, `skills/tracker/scripts/steward.py`, `workflows/codebase-polish.js`, `workflows/cross-verify.js`, `workflows/implement-run.js`.

**Retire tests (499 lines, subject to the mixed-test rule):** `tests/test_herdr_cli_contract.py`, `tests/test_herdr_orchestrate_contract.py`, `tests/test_setup_repo_contract.py`, `tests/test_tracker_placement_cases.py`, `tests/test_tracker_steward.py`.

**Caller/reference closure to switch:** `skills/setup-repo/SKILL.md`, `skills/tracker/references/steward.md`, `tests/test_command_docs.py`, `tests/test_herdr_cli_contract.py`, `tests/test_implement_run_workflow.py`, `tests/test_install_harness.py`, `tests/test_setup_repo_contract.py`, `tests/test_tracker_steward.py`.

**Manual invocation/ownership evidence:** scripts/install-workflows discovers and installs workflows/*.js by glob. The host dynamically imports meta/run exports. Update installer validation and installed workflow loader in this slice; literal grep alone misses this consumer.

### 48 — Public release checks and shared-source exit

**Owner:** Workshop; crates `cli, core; workflow caller API owned by Laptop`. **Budget:** 1.5 lane-days. **Prerequisites:** 06,07,08,10,14,17,24,29,38,46,47; owner APIs A1,A2,A3,A4,A5.

**Commands, entries and tools:** public-release-check; remove shared Python roots/excludes/JSONC only after last importing caller has moved; no MCP.

**Existing Rust:** cli::public_release + core::{roots,product_root_resolver,workspace_paths,paths,excludes,jsonc}: reuse contract policies, delete unused emulation.

**Contract / judges:** J1 release checker and root/environment catalogue; install/rollback both Macs; tests that inspect old Python structure need no port. **Missing cases / blockers:** Bounded Git tree/history object scan and growing-file stream, raw path rendering, protected sources and no interpreter/Node fallback with PATH stripped.

**Graph units:** `P/host_identity.py#{unit-01}`; `P/lib/jsonc.py#{unit-01}`; `P/lib/product_root_resolver.py#{unit-01}`; `P/lib/roots.py#{unit-01}`; `P/public_release_check.py#{unit-01…02}`; `skills/_shared/excludes.py#{unit-01}`; `skills/_shared/roots.py#{unit-01}`; `skills/_shared/workspace_paths.py#{unit-01}`; `integrate:d1f72d80b395`.

**Delete source (1,028 lines):** `P/lib/jsonc.py`, `P/lib/product_root_resolver.py`, `P/lib/roots.py`, `P/public_release_check.py`, `skills/_shared/excludes.py`, `skills/_shared/roots.py`, `skills/_shared/workspace_paths.py`, `P/host_identity.py`.

**Retire tests (2,278 lines, subject to the mixed-test rule):** `tests/test_shared_library_bootstrap.py`, `tests/test_import_path_contract.py`, `tests/test_ci_repository_assurance_policy.py`, `tests/test_host_identity.py`, `tests/test_product_root_resolver.py`, `tests/test_product_root_single_resolver.py`, `tests/test_public_release_check.py`, `tests/test_pytest_console_entrypoint.py`, `tests/test_split_root_contract.py`.

**Caller/reference closure to switch:** `.github/workflows/ci.yml`, `docs/adr/0019-installed-file-class-ownership.md`, `docs/runbooks/fabric-hosts.md`, `P/check-provenant-install.py`, `P/check_target.py`, `P/configure-fabric-mcp.py`, `P/fabric_hosts.py`, `P/instance_installation.py`, `P/model_route.py`, `P/provider_identity.py`, `P/public-release-check`, `P/telemetry.py`, `P/worktree.py`, `P/write-product-root-pointer.py`, `skills/_shared/delivery_run_invariants.py`, `skills/_shared/harness_profile.py`, `D/delivery_receipt.py`, `D/delivery_validation_common.py`, `D/reference_runs.py`, `D/software_delivery_validation.py`, `skills/implement/scripts/bind_merged_delivery.py`, `O/cf_dispatch.py`, `O/provider_confinement.py`, `skills/session/scripts/cleanup_run_artifacts.py`, `tests/test_cf_dispatch.py`, `tests/test_ci_repository_assurance_policy.py`, `tests/test_fabric_hosts.py`, `tests/test_host_identity.py`, `tests/test_import_path_contract.py`, `tests/test_model_route.py`, `tests/test_product_root_resolver.py`, `tests/test_product_root_single_resolver.py`, `tests/test_public_release_check.py`, `tests/test_pytest_console_entrypoint.py`, `tests/test_stale.py`, `tests/test_static_security_check.py`, `tests/test_telemetry.py`, `C/core/README.md`, `C/core/src/excludes.rs`, `C/core/src/host_identity.rs`, `C/core/src/jsonc.rs`, `C/core/src/product_root_resolver.rs`, `C/core/src/roots.rs`, `C/core/src/workspace_paths.rs`.

### 49 — Retire runtime-only tests and Node infrastructure

**Owner:** Laptop (workflow/fabric-*); Workshop (other listed crates); crates `workflow, fabric-*; Workshop cli/config hooks`. **Budget:** 1 lane-days. **Prerequisites:** 38,42,46,47,48; owner APIs A1,A2,A3,A16,A17,A18.

**Commands, entries and tools:** No new verbs. Delete runtime/fabric Node workspace, package scripts/deps/lockfiles after last Node caller; preserve independent judges.

**Existing Rust:** Remove Python/Node oracle Rust test adapters once their source owners are gone; no runtime interpreter probe in final Rust.

**Contract / judges:** CI-owned J1/J2/J3/lane conformance retained; CI alone may install Python for judges. **Missing cases / blockers:** Final tracked-source inventory, source refs/dynamic-loader audit, all fixture helpers checked for judge imports, no Node requirement in installing/running product.

**Graph units:** `C/cli/src/pending/generate_unicode.py#{unit-01}`; `C/core/scripts/generate_unicode.py#{unit-01}`; `C/telemetry/src/pending/python/generate_unicode.py#{unit-01}`.

**Delete source (761 lines):** `runtime/fabric/live-sandbox-smoke.py`, `C/cli/src/pending/generate_unicode.py`, `C/core/scripts/generate_unicode.py`, `C/fabric-store/tests/oracle.mjs`, `C/quota/src/tests/oracle.py`, `C/telemetry/src/pending/python/generate_unicode.py`, `C/workflow/tests/oracle.py`.

**Retire tests (1,503 lines, subject to the mixed-test rule):** `runtime/fabric/tests/atomic-write.mjs`, `runtime/fabric/tests/atomic-write.d.mts`, `runtime/fabric/tests/fixture-processes.test.ts`, `runtime/fabric/tests/helpers/fixture-processes.ts`, `runtime/fabric/tests/hermetic-tmpdir.ts`, `runtime/fabric/tests/spawn-fault-preload.mjs`, `tests/test_fabric_skill_params.py`, `tests/test_fabric_v1_contract.py`, `tests/test_harness_contract.py`, `tests/test_live_sandbox_smoke.py`, `tests/test_skill_audit_contract.py`, `tests/test_skill_reuse_rules.py`, `tests/test_static_skill_corrections.py`, `tests/test_support_helpers.py`, `tests/test_ui_ux_design_boundaries.py`, `tests/test_ui_ux_provenance_contract.py`, `tests/test_ui_ux_reference_contract.py`.

**Caller/reference closure to switch:** `runtime/fabric/README.md`, `tests/test_live_sandbox_smoke.py`, `tests/test_rust_port_graph.py`, `C/cli/src/pending/python_unicode.rs`, `C/confinement/src/pending/python_unicode.rs`, `C/core/README.md`, `C/core/src/py/unicode_data.rs`, `C/fabric-store/src/oracle.rs`, `C/quota/src/pending/python_unicode.rs`, `C/quota/src/policy_tests.rs`, `C/run/src/python_unicode.rs`, `C/telemetry/src/pending/python/python_unicode.rs`, `C/workflow/src/test_support.rs`.

**Manual invocation/ownership evidence:** Also remove runtime/fabric/package.json/tsconfig.json, root package workspace/dependencies/lockfile and Node install/audit workflow steps when their last runtime consumer is gone. Retain no package only to run obsolete private tests. These are future slice writes, not changes in this docs lane.

## Front-door route coverage

This is the complete current `main.rs` routing table, including rewrites and
special entries; new direct-entry verb spellings above remain proposals.

| Route / entry | Owning slices |
|---|---|
| `root`, `help`, `-h`, `--help`, `help <topic>` | 06; each verb slice supplies its help/argument cases |
| `lane` | 02,19 (reuse active S2) |
| Seat with no argv → MCP | 37–38 |
| `route`, `routes`, `help routes` rewrite | 13–14 |
| `capabilities` | 12 |
| `worktree` | 28 |
| `clean`, `reap` | 29 |
| `pr` | 08 |
| `lint`, `check` (special check dispatcher), `gate` | 07 |
| `telemetry`, `stats`, `retro` | 15–16,30 |
| `quota`, `profile` | 11,04 |
| `digest`, `classify`, `extract` (verb prefixed to utility owner) | 23 |
| `dispatch`, `batch`, `run` | 20–21 |
| `fabric` | 31–38; remaining parser table removed in 38 |
| `hosts`, `peer` | 36 |
| `lanes`, `events`, `graph` → `fabric <same verb>` | 32,35 |
| `lease acquire|release|verify|status|push` → `fabric landing-*` | 35–36 |

Fabric's parser also has `landing-wait` and internal `gate-exec`/`graph-owner`
entries outside the front-door alias list: 35 owns them. Its exact public
command set at `F/cli.ts:125` remains J1 scope; do not infer verbs from MCP
names. Installed direct Python/Node entry points are the additional scope in
each card, including UI wrappers absent from the generated graph.

## Shared owner APIs and first consumers

These are Rust contracts to settle/export, not permission to reproduce generic
Python/JS objects. Workshop publishes the lower owners; Laptop consumes them
without reversing R2. Inputs carry clocks/config/host observations explicitly;
libraries return typed errors/events. The binary/cli owns diagnostic and exit
rendering. Export only a behaviour proved necessary at the retained boundary.

| API | Owner and shape | First slice | Consumers / acceptance |
|---|---|---|---|
| A1 — byte and path boundary | core: OsString/PathBuf until a named record/CLI codec; pure root/identity/path/clock values; explicit float-second rounding only at retained timestamps | 01 | All slices; Unicode, non-UTF8 names, escapes, timestamps. Surrogateescape is a wire/path encoding rule where proved, never an internal string model. |
| A2 — checked records/codecs | records: checked typed serde decode, required-nullable fields, explicit ordered fields where observed, canonical/digest byte encoders, exact i64/u64 range refusal, JSONC and controlled YAML entry rules | 01 | All readers/writers; J2 plus byte fixtures. No indiscriminate deny_unknown_fields/defaults or generic CPython JSON clone. |
| A3 — safe host | supervisor: byte capture with separate caps/counts, stdin/stream sink, bounded deadlines, identity-bearing handles, safe publish/lock/entropy, process observer/watch lifetime | 02 | 04–49; custody, ENOSPC and sudden death conformance. Existing bounded text runner cannot replace raw/strict/live stream variants. Narrow config→supervisor safe-filesystem edge is allowed by merged R2; providers use confinement-supplied facts, not a forbidden direct supervisor import. |
| A4 — config and install snapshot | config: immutable resolved profile/environment/installation plan + explicit SafeFs-backed reconciliation, source marker and workspace digest | 04 | 05,06,11,12,15,17,38,48. No process global config mutation; help/root cheap. Before first library link (03), exercise already-landed digest's stale-library case. |
| A5 — Git evidence | cli/confinement compose supervisor raw Git runner with closed env and typed tree/porcelain/evidence observations; do not make a lower library import cli | 08 | Worktree, custody, release, delivery and federation. Shared parsing/policy goes core/confinement where required by R2; exact streaming bytes at edge. |
| A6 — review/workflow policy | workflow: typed review legs/panel plans/terminal judgments/portable context; no dict-of-functions or mutable import injection | 09 | Mission, delivery, tracker/workflow accelerators; J1/J2 review outcomes. |
| A7 — quota snapshot | quota: explicit-time availability/reset/cooldown/observation snapshot; safe probes provided as host results | 11 | Routing/run/telemetry; no routing→run cycle or provider signal on accounting failure. |
| A8 — provider profile and events | providers: registry/profile/argv/env/continuation/output event contracts; confinement returns resolved writable/config facts | 12 | Routing planning, observer, run. Seven adapters and exact requested/resolved/applied provenance. |
| A9 — route context | routing: catalogue/preference/live/quota values in, RouteDecision plus reason/provenance out; locked spread-state owned explicitly | 13 | 14,15,18,20,23,33; no cross-module globals. |
| A10 — telemetry sink/query | telemetry: immutable usage/event rows, transactional cursor/attribution adapter and bounded summary projections | 15 | Stats, delivery reports and federation; ledger owns IDs/dedup, no caller recomputes them. |
| A11 — run/attempt lifecycle | run + records + supervisor: admitted plan → typed events/results, cancel markers and status/output views; supervisor alone holds/signals descendants | 18 | 19–24 and Fabric. Planning types may start in providers/records to avoid run→workflow→run cycle. No second writer, no killing on silence/bookkeeping failure. |
| A12 — extract schema validation | records-owned contract with cli rendering, validation semantics for accepted schema drafts | 23 | Utility extraction. **Blocked:** current Ajv call is real runtime; allowlist has no JSON Schema crate. Chair resolves an implementation within R6 or seeks ADR/dependency authority; no silent subset validator or retained Node fallback. |
| A13 — receipt validation/mutation | workflow + records: typed validation context, transactional receipt deltas, artifact hashes and Git bindings | 25 | 26–30 and retention; no user acceptance inferred from state or green CI. |
| A14 — Fabric store | fabric-store: schema/migrations/transactions, explicit identity view, clocks/IDs/claims/leases/cursors; runtime converts its richer identity at the edge | 31 | Runtime supplies entropy, directory creation and isolated diagnostic snapshots through supervisor; store's R2 supervisor dependency is safe locking only. Never store→runtime; no transaction held across wait, one connection per owning thread. |
| A15 — Fabric runtime facade | fabric-runtime: identity/run/status/output/execution/graph/federation operations over run/store; bounded waits and cancellation tokens | 32 | 33–38 and MCP; retired TS→Python bridge disappears. |
| A16 — MCP transport/results | fabric-mcp: JSON-RPC frames/tool schemas/content/error rendering and bounded worker transport | 37 | Seat default process setup/registration in 38. All 20 tool names; no SDK, Tokio or daemon. |
| A17 — browser/UI host | workflow policy + supervisor process boundary; typed DOM/findings/source/journal/HTTP effects rather than JS private objects | 40 (parser), 41–46 (browser) | **Unverified/blocked design:** browser injected JS and vendor screenshot are live; native Rust cannot simply replace script tags. Chair must settle compatible Rust/browser execution and dependency/toolchain bounds before these slices run; hard calls use council. A WASM target/bridge is a proposal, not approved here. |
| A18 — installed workflow host | workflow: typed request/result/metadata for accelerator execution; installer/host loads Rust operation instead of JS exports | 47 | **Unverified** external host loader. Capture real installed host fixture first; do not delete JS because Python import graph omitted it. |

`graph.json` currently encodes old source cycles, including the large
`integrate:d1f72d80b395` cycle and the two Fabric integration gates.
The proposed APIs break those implementation cycles. The serial graph owner
must regenerate dependency edges after interfaces are accepted **before**
using ready.py to dispatch the affected cut-over rows. This docs lane changes
neither generated graph nor `ported.txt`. `integrate:496bc5e66c6b` closes at 33,
`integrate:5ef42cbe0051` at 32 and the last large-cycle closure is 48;
all contract behaviours across those members must pass at their new owners.
Do not mark a source unit done merely because its interface is inert.

## Inert Rust to drop or simplify

The following search hits are obligations to inspect and reduce, not proof all
uses are private. `pending` is not an owner API. Keep exactly one implementation
in core/records only when J1/J2/J3 or a provider wire fixture demonstrates the
behaviour; delete internal-only emulation and its oracle tests outright.
No new pending copy is permitted by the lane brief. Route names below refer to
files under `tools/front-door/crates/`.

| Rust path/module | Evidence / disposition | Removal slice |
|---|---|---|
| `cli/src/projection.rs` + `cli/src/lib.rs` export | `rg 'projection::|mod projection|pub mod projection'` yields its export and module-local tests; oracle imports the dead Python helper. No front-door runtime use. Delete both implementations and private tests, retain independent risk-table checks. | 00 |
| `routing/src/pending.rs` | Local `python_strip`, `python_repr`, digit lookup, checked JSON and ISO readers duplicate core/records. Remove generic internal coercion; contract-specific diagnostic/record uses go to A1/A2. | 13–14, cleanup PR only for a proved unused remainder |
| `confinement/src/pending.rs`, `confinement/src/python.rs` | Local repr/coercion and model-object adapters accompany pending filesystem seams. Reuse SafeFs and typed grants; no Python-value model inside confinement. Preserve only demonstrated refusal bytes at cli edge. | 17 |
| `providers/src/pending.rs`, `providers/src/ordered_json.rs` | Unicode digits/int coercion and negative-zero/ordered decoder bridge. Keep exact integer rejection and provider wire bytes through A2; remove repr/truthiness/int machinery from internal options. | 12,18,22 |
| `config/src/pending.rs`, `config/src/pending/ordered.rs` | #1336 already removed config python_unicode.rs and local resolver/publisher. Remaining parse_int, path_sort_key, JSON encoders and open-value adapters must be narrowed to manifest/config wire needs; typed policies need none of the generic Python semantics. | 04–06; cleanup PR for any independently dead exports |
| `records/src/pending.rs` | `python_strip` and temporary open-value/path encoders. Records may own canonical bytes, not a universal Python interpreter. Fold proved codecs into A2 and remove unused emulation. | 01,19,25 |
| `fabric-runtime/src/pending.rs` | `trim_ecmascript`, checked JSON, root fallback and recursive value `python_repr`. core::js already exports trim. Delete the Python-owner/policy diagnostic adapter with bridge cut-over; use typed runtime results. | 32–33 |
| `fabric-store/src/pending.rs` | JS trim/number conversion/UTF-16 slice/Date formatting plus Identity/IdSource. Core::js already implements several wire behaviours. Drop internal coercion; retain only tool/store-boundary semantics demonstrated in J3/SQLite fixtures. Identity is a store input view, not a reason to import runtime. | 31 |
| `run/src/python_unicode.rs`, `run/src/pending.rs`; `cli/src/pending.rs` + `pending/python_unicode.rs` (where present) | Copied Unicode/repr/digit/splitlines semantics. Replace internal lease/run fields by typed inputs and ranges. Move only visible CLI parse/byte cases to A1/A2. Delete duplicate table generators when the tables go. | 20–24,48; remaining dead copies in a focused cleanup PR |
| `telemetry/src/pending.rs`, `pending/{decode,numeric,python}.rs`, `pending/python/python_unicode.rs` | JSON/hash/numeric/truth/equality/letter/float-time emulation. Keep exact event IDs, cost rounding and serialized rows at A2; use native typed reducers otherwise. | 15–16,30 |
| `supervisor/src/pending.rs` | Pending process/path/publication contracts must use safe APIs and typed identity. Delete policy emulation; do not delete custody machinery or SafeFs because it resembles an old implementation. | 02,19,22 |
| `core/src/pending.rs`, `core/src/py/*`, `core/src/js.rs` | Inventory the uses after last caller migration. E.g. surrogateescape path encoding may be externally required, not automatically dead. Remove runtime interpreter probing and generic repr/strip/digit tables whose only consumers were retired emulation tests; freeze contract encoders independently. | 48–49 cleanup, after boundary-case audit |
| `*/src/oracle.rs`, `*/src/test_support.rs`, `*/tests/oracle.*`, pending Unicode generators | Differential scaffolding calls Python/Node and can keep interpreters required in cargo test. Delete with the corresponding old source owner; keep language-neutral fixtures/Rust policy assertions. Never count an oracle as product reuse. | With owner slice; 49 proves none remain |

A cleanup PR is a small follow-up for dead Rust only; it cannot defer deleting
an old Python/TS owner or leave a second runtime algorithm after cut-over.
Changes to the root workspace, dependency features and lockfile remain one
serial integration owner's work. Existing dependency allowlist/toolchain pin
stay in force; no slice can add a crate to make its estimate fit.

## Judge gates and promotion

- J1 is `tests/golden/run.py --verb <verb>` (or `--case`), with
  `PROVENANT_GOLDEN_BIN` selecting the explicit candidate; its paired
  `tests/golden/diff.py` compares isolated old/new paths. Current golden
  README says most provider verbs have help/validation coverage only, route
  resolve is help, and gate is help only. A verb name in the manifest does
  not prove its behavioural cut-over. Add the missing cases above against
  the pinned old implementation and prove deliberate mutants fail first.
- J2 is `tests/test_record_schemas.py` with `schemas/` and writer fixtures.
  Existing future launch/observer/state examples are explicitly provisional
  in schemas/README.md. Settle their owner shapes before treating them as
  release gates. Logical DB equivalence and byte-sensitive digest fixtures
  complement JSON Schema; schema validity never proves canonical bytes.
- J3 exists at `tests/mcp_judge/`, documented in its README; run pytest there
  with `PROVENANT_MCP_SERVER_CMD` selecting the isolated Rust candidate.
  It records initialise/tools-list and all tools' happy/error responses,
  but dispatch currently stops at validation. Add fake-owner execution,
  concurrency/cancel/backpressure/malformed-frame coverage before 33/37.
- `tests/lane_conformance/` is retained. The current entry is still Python
  dispatch for both S2 selectors. In 20 change that launch to the real Rust
  dispatch while preserving FIFO-controlled providers and independent process/
  retained-file assertions. Exercise Linux and macOS arm64, failure and owner
  death; zero unexplained healthy-process kills or lost terminal records.

Every slice pins rulebook/source/oracle/binary/config/judge revisions, records
old-pass/mutant-fail and new-pass, and gets principal contract/diff review plus
chair-arranged review pressure under HARNESS.md. Contract-visible divergences
need chair disposition; internal redesign does not. No stable install,
credentials or live provider run is changed to make a judge pass. Run only
fast targeted checks locally; exact-head required CI gates landing. Merge
main when it moves, never rebase; lanes open PRs and do not arm auto-merge.

At final promotion, stop admission, drain active owners, back up and prove
both binaries reopen a copy of retained state; rehearse candidate install and
rollback on both Macs. Never run old and new owners over the same live attempt
or SQLite database. Rollback restores the pinned install/pointers (or old
source from Git after deletion), not an old DB over new writes. A state
migration that cannot roll back or an unapproved dependency amendment returns
to the user under R11. Missing judge cases, dynamic callers, A12/A17/A18 and
API cycles block only their affected slices, not prune/writing/review leaves.

## End state and remaining CI Python

All product commands, library policy, provider mechanics, hooks, installed
workflow/tracker operations, Fabric runtime/store/MCP and UI tooling are owned
by the single `tools/front-door` Rust workspace/binary. No Node runtime,
Python execution owner, TS→Python bridge, fake CF_DISPATCH transport, optional
fallback implementation or runtime oracle survives. Small POSIX wrappers may
exec the front door. Installed explicit interpreter commands must have been
rewritten and tested. UI browser assets/workflow JS are not exemptions:
A17/A18 must be settled and their replacements proved before claiming the
whole-project port complete.

The permitted remainder is **CI/development tooling only**, with removal
conditions:

| Retained tooling | Why / condition for removal |
|---|---|
| `scripts/static-security-check.py`, `tests/test_static_security_check.py` | R6 explicit retention while any tracked Python remains, including the Python CI judges retained below. It therefore remains in this plan's end state. Removal is conditional on a separate judge/dev-tool migration or retirement leaving no other tracked Python, plus passing Rust unsafe/command-construction controls; remove the checker and its tests in that final cohort. |
| `scripts/lint-timebomb-fixtures.py`, `tests/test_timebomb_fixture_lint.py` | Scan remaining Python/TS test fixtures. Remove when no `tests/*.py` and no `*.test.ts` remain; Rust textual date gate from 07 already owns Rust tests. |
| `docs/specs/rust-port/derive_graph.py`, `docs/specs/rust-port/ready.py`, `tests/test_rust_port_graph.py`, `tests/test_rust_port_layering.py` | Port-planning/build-layer checks, no shipped command. Delete planning generator/ready/tests when all port assignments and retired units are closed; keep a Rust/textual layering check if still needed. Do not port Python AST parsing. |
| `tests/golden/*.py`, `tests/mcp_judge/*.py`, `tests/lane_conformance/*.py`, `tests/test_record_schemas.py`, their fixture/provider support, `tests/conftest.py` | Independently owned CI judges remain language-neutral in contract despite Python runners. Replace/retire the runner only in a separate judge-owned change proving the same old-pass/mutant-fail boundary. Product cut-over does not depend on rewriting CI to Rust. |
| The two retained projection risk-policy assertions and remaining docs/policy assurance tests | CI policy/config checks can stay Python if they invoke Rust or inspect data/text. No importing deleted product modules. Remove when an equivalent CI assertion replaces them or the policy is retired. |

Retained helper closure currently includes the paths below; **unverified** as
an eventual minimum because fixture code still imports Python owners. Each
owner slice must break those imports and either keep the helper CI-only or
remove it after its last judge caller. The closure may shrink; it cannot
justify retaining product code merely because a test imports it.

`tests/cf_dispatch_support.py`, `tests/conftest.py`, `tests/fixtures/current-routing-eval/dispatch_fixture.py`, `tests/fixtures/current-routing-eval/provider_fixture.py`, `tests/golden/diff.py`, `tests/golden/run.py`, `tests/golden/test_diff.py`, `tests/golden/test_golden_cli.py`, `tests/lane_conformance/__init__.py`, `tests/lane_conformance/conftest.py`, `tests/lane_conformance/fake_provider.py`, `tests/lane_conformance/support.py`, `tests/lane_conformance/test_s2_capture.py`, `tests/lane_conformance/test_s2_lifecycle.py`, `tests/lane_conformance/test_s2_rust_faults.py`, `tests/lane_conformance/test_seed.py`, `tests/lane_stub.py`, `tests/mcp_judge/client.py`, `tests/mcp_judge/conftest.py`, `tests/mcp_judge/test_transcripts.py`, `tests/support/__init__.py`, `tests/support/confinement.py`, `tests/support/dispatch.py`, `tests/support/fake_cli.py`, `tests/support/front_door.py`, `tests/support/git.py`, `tests/support/modules.py`, `tests/support/proc.py`, `tests/support/quota.py`, `tests/support/runs.py`, `tests/test_record_schemas.py`, `tests/test_rust_port_graph.py`, `tests/test_rust_port_layering.py`, `tests/test_static_security_check.py`, `tests/test_timebomb_fixture_lint.py`.

The Node package infrastructure is removed only after Fabric, UI and installed
workflow callers have left it; CI is not a reason to keep TS product sources
or their private tests. Runtime-only leftover tests without a live product
subject are retired in 49; docs/security/policy assertions in those mixed
files are first retained as data/text/Rust-command checks. Final evidence is
`git ls-files` plus the repeated caller/env/loader audit and a fake-provider
install/MCP/UI smoke with no Python or Node available to the product.
