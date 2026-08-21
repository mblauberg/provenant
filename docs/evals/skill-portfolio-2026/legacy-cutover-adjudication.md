# Legacy cutover adjudication (D2)

**Status:** read-only adjudication, 2026-08-02. No code changed.
**Repo:** `$AGENTS_HOME/.worktrees/baseline-integration`, branch `baseline-integration`.
**Question:** the D2 cutover was recorded blocked on an apparent test-suite
contradiction. Does the contradiction exist, and what does D2 actually implicate?

> **Historical pre-ADR-0020 evaluation.** The removed daemon paths and test
> counts below are retained review evidence, not current implementation
> guidance. See [ADR 0020](../../adr/0020-retire-the-daemon-fabric.md) and
> [`runtime/fabric/README.md`](../../../runtime/fabric/README.md).

---

## 0. Executive summary

There is **no contradiction**. The four cited test locations constrain four
different artifacts with two different subjects, and only one of the four is
about this repo's own change style at all. The block dissolves.

The real cutover is **larger than the single known shim** but **smaller than a
doctrine rewrite**: roughly **1,450 removable lines**, dominated by one cluster
(the Agent Fabric seat `originKind` back-compatibility layer) that alone is
about 43 per cent of the total. Nothing in the removable set breaks this
machine's ability to install or upgrade the harness, and that was checked
against live on-disk state rather than inferred.

---

## 1. Subject map

The governing distinction is between:

- **Subject A, advice:** artifacts that tell the harness what to do when it
  works on *someone else's* system, where old and new consumers genuinely
  coexist. Migration doctrine is correct and necessary here.
- **Subject B, this repo:** artifacts that govern changes to *this*
  repository's own code and local state, where ADR 0002 records direct cutover.

### 1.1 `tests/test_skill_principle_and_safety_contracts.py:79-91`

**Constrains:** `skills/implement/SKILL.md` and
`skills/implement/references/migration-compatibility.md`.

**Subject: A (advice).** This is a skill reference. The `implement` skill is the
harness's lifecycle owner for *approved software changes in whatever repository
it is pointed at*. The reference's own first line scopes itself conditionally:

> "Use for schema, data, protocol, API, configuration, dependency or storage
> changes where old and new producers or consumers may coexist."

**Exact assertions** (test name
`test_migrations_preserve_mixed_version_safety_and_expire_compatibility_paths`):

```python
    skill = squash(text("skills/implement/SKILL.md"))
    migration = squash(text("skills/implement/references/migration-compatibility.md"))
    for phrase in (
        "mixed-version window",
        "expand, migrate, contract",
        "usage-zero evidence",
        "expiry owner",
        "containment",
    ):
        assert phrase in f"{skill} {migration}"
    assert "Every migration needs a down migration" not in migration
```

Note the shape of the last line. The test is not demanding migrations; it is
demanding that migration advice, *when given*, carry an expiry owner and
usage-zero deletion evidence, and it explicitly forbids the over-broad claim
that every migration needs a down migration. This is a test that compatibility
paths must be *disposable*, which is D2-aligned, not D2-opposed.

### 1.2 `tests/test_delivery_contract.py`

The brief cites lines 438-453. Those lines are the body of a delegation fixture
inside
`test_authority_delegation_maps_and_refuses_broadened_scope`: `irreversible_actions`,
`network`, `expires_at`, `budget`. They concern authority-envelope narrowing and
say nothing about compatibility or migration. **The citation appears to be an
error.**

The compatibility-relevant test in that file is at **lines 547-562**:

**Constrains:** `skills/deliver/scripts/delivery_policy_validation.py`, this
repo's own delivery-receipt validator.

**Subject: B (this repo).** This is executable code in this repo that reads
receipts this repo writes.

**Exact assertions** (test name
`test_delivery_v1_records_typed_fabric_relationships_without_breaking_legacy_receipts`):

```python
    legacy = copy.deepcopy(candidate)
    del legacy["fabric_relationships"]
    module.validate(legacy, ROOT)
```

This one **is** a genuine legacy reader in this repo's own code, and it is a
real D2 target. It is item 8 in the inventory below. It is two lines of
implementation.

### 1.3 `tests/test_release.py:603-664`

**Constrains:** `skills/release/scripts/validate_release.py`, via the local
`validate_policy` helper at `tests/test_release.py:193`.

**Subject: A (advice).** Decisive evidence is the fixture's own target. The
second test in the range sets:

```python
    receipt["target"] = {
        "id": "provider:prod-au", "kind": "environment",
        "environment_tier": "production", "disclosure": "internal",
    }
```

`provider:prod-au` is an external production environment. This policy governs
receipts for releases the harness performs *against other systems*. It is not a
statement about how this repository changes its own files.

**Exact assertions:**

```python
    errors = validate_policy(receipt, "ready")
    assert "non-backward-compatible change requires a compatibility window" in errors
    assert (
        "destructive or non-backward-compatible change requires explicit irreversible-action authority"
        in errors
    )
```

and in `test_destructive_migration_keeps_global_compatibility_and_recovery_gates`:

```python
    receipt["change_impact"] = {
        "state_change": "destructive",
        "compatibility": "non-backward-compatible",
        "ordered_steps": ["expand", "migrate", "contract"],
        "compatibility_window": "old and new readers supported through the observation window",
        ...
    }
```

Read carefully, this is a **gate on non-backward-compatible releases, not a ban
on them**. It says: if you declare a change non-backward-compatible against a
production environment, you must also declare a window and hold explicit
irreversible-action authority. A direct cutover in this repo is not a release
against `provider:prod-au` and never enters this validator.

### 1.4 `tests/test_ci_repository_assurance_policy.py:951-960`

**Constrains:** `.github/pull_request_template.md`.

**Subject: B (this repo).** The PR template governs pull requests raised against
this repository.

**Exact assertions:**

```python
    for evidence in (
        "direct cutover",
        "no legacy reader",
        "compatibility bridge",
        "migration preflight",
        "rollback or forward-repair",
        "trigger or query-plan evidence",
    ):
        assert evidence in template
    assert "historical formats remain readable" not in template
```

The template text those assertions match, at
`.github/pull_request_template.md:40-45`:

> "Contract and cutover: current schema, protocol and configuration owners
> checked; any pre-release **direct cutover**, regeneration or reset path
> recorded; **no legacy reader**, **compatibility bridge** or historical-format
> promise **added**; persistence or schema changes carry **migration
> preflight**, **rollback or forward-repair**, and **trigger or query-plan
> evidence**."

Two things matter here. First, all six required strings occur in a single
sentence: the template requires the PR author to *account for* cutover and for
migration preflight in the same breath, so "migration preflight" appearing
alongside "direct cutover" is one compound gate, not two competing doctrines.
Second, the prohibition is on what is **added**. The template forbids
introducing new legacy readers; it does not assert that existing ones must be
preserved.

### 1.5 Additional locations found (not cited in the brief)

| Location | Constrains | Subject | Note |
|---|---|---|---|
| `docs/adr/0002-capability-compiled-execution-authority.md:52-56` | The D2 decision itself | B | The authoritative record. Quoted in full in section 2. |
| `runtime/agent-fabric-console/tests/pre-release-baseline.test.ts:5-11` | Console production sources | B | An **existing D2-enforcing gate**. Forbids the literal tokens `legacy-compatibility`, `strict-v1`, `STRICT_V1_OPTIONAL_FEATURES`, `legacy-fallback` from appearing in nine named production source files. |
| `skills/deliver/references/contract.md:66` | This repo's delivery contract | B | "`application/x-git-archive` receipts remain readable only with their SHA-256 ..." An explicit legacy-reader promise in this repo's own contract. A D2 target, item 7 below. |
| `skills/ui-ux-design/scripts/load-context.mjs:41,63-92` | User projects' `.impeccable.md` | A | Legacy format in *the projects the skill reviews*. Not a D2 target. |
| `skills/ui-ux-design/scripts/impeccable-paths.mjs:38-53` | This repo's own skill scripts dir | B | `getLegacyLiveConfigPath(scriptsDir)` reads `config.json` from the skill's own directory in this repo. A D2 target, item 6 below. |

The console baseline gate is the strongest independent confirmation of the
reading. This repository already runs an automated test that forbids legacy and
compatibility tokens in its own production sources, and that test coexists
peacefully with the `implement` skill's migration doctrine. If the two subjects
were genuinely in conflict, that gate could not already be green.

---

## 2. Verdict

**No real contradiction exists.**

The apparent conflict was a **subject confusion between doctrine the harness
teaches and policy the harness lives by**. Three of the four cited locations
(1.1, 1.3, and the mis-cited 1.2 range) constrain artifacts whose subject is
work performed on other systems, where mixed-version windows are real. One
(1.4) constrains an artifact whose subject is this repository's own pull
requests. Neither group asserts anything the other denies.

Two further observations close the question rather than merely deflecting it:

1. **The doctrine is conditional, and its precondition is unmet here.**
   `skills/implement/references/migration-compatibility.md:3-4` scopes itself to
   changes "where old and new producers or consumers may coexist". ADR 0002
   records the exact reason that precondition fails for this repo:

   > "**Direct cutover, no legacy bridge** (human directive, overriding
   > codex-pair's proposed `LegacyAuthorityInputV1` quarantine): the repo is
   > pre-release with no external consumers; migrate all callers, tests and
   > stored state to V2 in the authority-contract cutover. Pre-existing stored
   > authorities are regenerated or the local pre-release state is reset, no
   > dual parser is retained."
   > (`docs/adr/0002-capability-compiled-execution-authority.md:52-56`)

   No external consumers means no coexistence window, so the skill's own gating
   clause excludes this repo. Applying the doctrine here would be a misreading
   of the doctrine, not compliance with it.

2. **The two texts agree on the hard part.** The migration reference already
   demands that "every compatibility adapter, flag, dual path or deprecated
   field" carry "an expiry owner, removal condition and latest review date", and
   that deletion require "usage-zero evidence"
   (`skills/implement/references/migration-compatibility.md:17-20`). The PR
   template demands no *new* legacy readers. These are the same policy at two
   different tightness settings for two different risk profiles.

**What the block actually was:** the recorded blocker treated "this file
contains the word migration" as equivalent to "this repo mandates migrations".
It did not check whose code each artifact governs, and it did not read the
`implement` reference's opening scope line or ADR 0002. The correct action is to
close the block and proceed with the inventory in section 3, leaving all four
cited tests untouched except the one at
`tests/test_delivery_contract.py:547-562`, which pins a real legacy reader and
should be rewritten (see the cutover table).

**Nothing needs to change in `skills/implement/`, `skills/release/`, or
`.github/pull_request_template.md`.**

---

## 3. Inventory of what D2 actually implicates

Ranked by estimated lines removed. Only this repo's own executable code, config
and the tests that pin it are counted. Doctrine prose in `skills/**/*.md` that
advises other projects is excluded, per section 1.

Liveness was checked against real on-disk state on this machine where the code
touches installed state; see section 5.

### Item 1. Agent Fabric seat `originKind` back-compatibility cluster

**~626 lines (136 source, ~490 test). The single largest item, 43 per cent of
the total.**

Seat metadata written before the `originKind` field existed is still accepted,
and a sidecar `legacy-bootstrap.json` marker records which generation was a
bootstrap seat so that origin-less metadata can be retro-classified.

| File and lines | What it does |
|---|---|
| `runtime/agent-fabric/src/cli/seat-store.ts:16` | `originKind?: "bootstrap" \| "provisioned"` optional only so pre-field metadata parses |
| `runtime/agent-fabric/src/cli/seat-store.ts:69-73` | `LegacyBootstrapGenerationMarker` type |
| `runtime/agent-fabric/src/cli/seat-store.ts:297-309` | `parseLegacyBootstrapMarker` |
| `runtime/agent-fabric/src/cli/seat-store.ts:330-346` | `readLegacyBootstrapSeatGeneration` |
| `runtime/agent-fabric/src/cli/seat-store.ts:348-382` | `markLegacyBootstrapSeatGeneration`, writes `legacy-bootstrap.json` |
| `runtime/agent-fabric/src/cli/mcp-bootstrap.ts:35,613,659,661-681,698-705` | Dual-write: retries the seat install without `originKind` when the daemon rejects the new shape, then records a `legacy-bootstrap-provenance` ledger entry |
| `runtime/agent-fabric/src/cli/mcp-peer-provision.ts:27,346-349,356-363` | Falls back to the marker when `metadata.originKind` is absent |
| `runtime/agent-fabric/src/cli/mcp-provision.ts:78,445-446,470-478` | `originKind` typed `\| null` purely to represent "absent in old metadata" |
| `runtime/agent-fabric/src/mcp/credentials.ts:8,256-259,290-293` | `verifiedLegacyBootstrapSeat`: treats metadata with no `originKind` as a bootstrap seat if the marker generation matches |
| `runtime/agent-fabric/src/cli/status.ts:399` | `originKind: value.originKind ?? "legacy-bootstrap"` synthesised display value |
| `runtime/agent-fabric/src/lifecycle/lifecycle-receipt.ts:37-42,153-159` | `"legacy-bootstrap-provenance"` lifecycle action variant and its parser arm |

Tests: `runtime/agent-fabric/tests/unit/mcp-bootstrap-legacy-provenance.unit.test.ts`
(271 lines, entirely this subject),
`tests/unit/seat-store.unit.test.ts:9,21-55`,
`tests/unit/mcp-credentials.unit.test.ts:10,277-330`,
`tests/system/lifecycle/zero-touch-lifecycle-receipt.test.ts:317-350`,
`tests/system/lifecycle/mcp-zero-state-bootstrap.test.ts:14,214-305`.

**Liveness: dead on this machine.** All 27 live seat metadata files under
`$HOME/.local/state/agent-harness/fabric/seats/*/generations/<current>/` carry
`originKind`, and no `legacy-bootstrap.json` marker exists anywhere in the
state directory. The tolerance path cannot currently be taken here.

### Item 2. Skill-rename migration machinery

**~412 to ~612 lines (162 source and config, 250 to 450 test).** The wide band
is an UNRESOLVED item; see section 6.

| File and lines | What it does |
|---|---|
| `scripts/manage_installation.py:335-453` | `_load_renames`, `_prepare_renames`, `_rollback_renames`, `_apply_renames`: repoints installed skill symlinks from old names to new ones and merges manifest history |
| `scripts/manage_installation.py:531-558,640,650` | `reconcile` action wiring and argparse surface for `--renames` |
| `config/skill-renames.json:1-17` | The rename registry. All 12 `from` names refer to skills that **no longer exist in this repo**: `change`, `humanise-text`, `multi-agent-orchestration`, `skill-optimizer`, `write-a-skill`, `vercel-react-best-practices`, `skill-audit`, `skill-authoring`, `frontend-design`, `frontend-review`, `autonomous-lab`, `github-setup` |

Tests: `tests/test_install_skills.py`, 12 rename or reconcile test functions at
lines 173, 341, 685, 739, 755, 792, 834, 859, 883, 1112, 1145, 1197.

**Liveness: semi-dead.** Not invoked by `scripts/install-harness`,
`scripts/install-skills` or any CI workflow. Reachable only by a hand-run
documented at `MAINTAINING.md:119-122`. This is the clearest single instance of
"a migration path for this repo's own installed state", which is exactly what
D2 forbids.

### Item 3. `provenant` command legacy-link shim

**~103 lines (23 source, ~80 test).** This is the shim named in the brief.

| File and lines | What it does |
|---|---|
| `scripts/install-harness:116,121,156` | `legacy_provenant_target="$instance_root/scripts/provenant"` threaded into classify and publish |
| `scripts/install-provenant-command.py:20` | `"legacy-link"` in `OWNED_STATES` |
| `scripts/install-provenant-command.py:90-114` | `_owned_snapshot` treats a symlink pointing at the old install layout as harness-owned rather than a foreign collision; `_normalised_link` exists only for this check |
| `scripts/install-provenant-command.py:169,174,210,256,264,271,276` | `legacy_target` parameter threaded through `publish`, argparse and the CLI dispatch |

Tests: `tests/test_install_provenant_command.py`, 26 `legacy` references across
tests at lines 36-64, 80-104, 114-136, 155-176, 191+.

**Liveness: dead on this machine.** `$HOME/.local/bin/provenant` is a regular
file (4115 bytes, mode `-rwxr-xr-x`), not a symlink, so `classify` returns
`existing` or `managed-file` and can never return `legacy-link` here.

### Item 4. Instructions-file doctrine migration in the installer

**~65 lines (35 source, ~30 test).**

| File and lines | What it does |
|---|---|
| `scripts/install-harness:193-197` | `legacy_doctrine="$PRODUCT_ROOT/AGENTS.md"` |
| `scripts/install-harness:200-222` | `migrate_instructions()`: an `awk` helper that rewrites the old product-root `AGENTS.md` path to the instance-root path inside the user's `CLAUDE.md`/`AGENTS.md` instructions file |
| `scripts/install-harness:231` | Extra symlink arm accepting the legacy doctrine target |
| `scripts/install-harness:237-242` | The branch that triggers the migration and reports `instructions migrated=` |

Tests: `tests/test_install_harness.py:1185-1198`.

**Liveness: dead on this machine, twice over.** First, the layout is fused
(`PRODUCT_ROOT` and `instance_root` are both `$HOME/.agents`), so
`legacy_doctrine` and `instance_doctrine` are the same string and the awk
rewrite is a byte-for-byte no-op; the code comment at
`scripts/install-harness:195-196` says exactly this. Second, the live
`$HOME/.claude/CLAUDE.md` contains **neither** literal doctrine path (verified:
zero grep matches for `.agents/AGENTS.md` or `.agents/HARNESS.md`), so the
guards at lines 231, 235 and 237 all fail today and control already reaches the
`else` arm at line 243.

### Item 5. `change_gates.py` partial-revert fallbacks

**~60 lines (48 source, ~12 test).**

`scripts/change_gates.py:119-172` defines three wrappers that each do
`globals().get("_structured_…")` and fall back to a duplicated inline
implementation:

```python
def classify_failure(returncode: int, output: str) -> FailureClass:
    """Expose the explicit legacy classifier, with a partial-revert fallback."""

    implementation = globals().get("_structured_classify_failure")
    if implementation is not None:
        return implementation(returncode, output)
    # ... 12 lines of duplicated classifier ...
```

**These fallbacks are statically unreachable.** The `_structured_*` names are
imported unconditionally in **both** arms of the try/except at
`scripts/change_gates.py:38-40` and `:52-54`, so if the module imports at all,
they are bound. Confirmed empirically:

```
structured classify bound: <function classify_failure at 0x10523d9b0>
structured runner bound:   <function runner_for_command at 0x10523dfe0>
structured run bound:      <function run_command at 0x10523e820>
```

Removable: the dead classifier body at `:125-136`, the dead `subprocess.run`
body at `:154-173`, the `else None` at `:141`, the defensive
`globals().get("runner_for_command")` at `:685-686` (a lookup of a function
defined in the same module), and the `except TypeError` retry at `:693-699`
that calls `run_command` without the `runner` keyword for a signature that no
longer exists.

All of it was introduced in a single commit, `908bd4fd1` ("fix(gates): classify
structured test outcomes (#622)"), as scaffolding for the split of
`change_gates.py` into `change_gate_runner.py` and `change_gate_reports.py`.

Test-side counterparts: `tests/test_change_gates.py:35-38,57-60`, helpers that
tolerate a reverted implementation. **These are UNRESOLVED**; see section 6.

Note: `scripts/change_gate_runner.py:183` `_run_legacy` is **not** a D2 target
despite the name. It is the live path for unstructured commands with no
report-file runner, reached whenever `runner is None` at
`scripts/change_gate_runner.py:309-310`. It is misnamed, not legacy.

### Item 6. `ui-ux-design` skill's own legacy layout

**~30 lines (source only).**

| File and lines | What it does | D2? |
|---|---|---|
| `skills/ui-ux-design/scripts/impeccable-paths.mjs:38-40,49-52` | `getLegacyLiveConfigPath(scriptsDir)` reads `config.json` from the skill's own scripts directory in **this repo**, a former layout | Yes |
| `skills/ui-ux-design/scripts/impeccable-paths.mjs:60,65,95` | `getLegacyLiveServerPath` dual read of `server.json` | Yes |
| `skills/ui-ux-design/scripts/impeccable-paths.mjs:104-114` | `getLegacyLiveSessionsDir` / `getLegacyLiveAnnotationsDir`, reading `.impeccable-live/` in the target project | UNRESOLVED |
| `skills/ui-ux-design/scripts/live-session-store.mjs:3,9,25-26,32,37-39,68` | Dual-read of session journals plus a **copy-forward migration** at `:37-39` | UNRESOLVED |
| `skills/ui-ux-design/scripts/impeccable-paths.mjs:21-22`, `load-context.mjs:41,63-92`, `live-server.mjs:44-45,199-200` | Legacy `DESIGN.json` and `.impeccable.md` in **the user's projects** | **No, subject A** |

The last row is the same subject confusion as the doctrine question, one level
down: these read formats belonging to the projects the skill reviews, not to
this repo. They must be left alone.

**Liveness of the D2 rows: dead on this machine.** No `.impeccable-live`
directory exists anywhere under `$HOME` at depth 3, and no `config.json` sits in
the skill's scripts directory.

### Item 7. Legacy Git archive artifact media type

**~25 lines (10 source, ~15 test).**

| File and lines | What it does |
|---|---|
| `skills/deliver/scripts/software_delivery_validation.py:148-156` | `legacy = media_type == "application/x-git-archive"`, then a branch requiring a SHA-256 digest for the legacy shape and forbidding digest fields for the current `application/x-git-revision` shape |
| `skills/deliver/scripts/software_delivery_validation.py:174-182` | Re-derives the legacy archive with `git archive --format=tar` and compares digests |
| `skills/deliver/scripts/software_delivery_validation.py:199` | `GIT_ARTIFACT_FIELDS \| ({"digest"} if media_type == "application/x-git-archive" else set())` |
| `skills/release/scripts/validate_release.py:228` | Mirror check on the release side |
| `skills/deliver/references/contract.md:66` | The written promise: "`application/x-git-archive` receipts remain readable only with their SHA-256 ..." |

Tests: `tests/test_software_delivery_continuity.py:126`.

This is a textbook dual reader in this repo's own validator, plus an explicit
historical-format promise in this repo's own contract document. Under D2 both go.

### Item 8. `fabric_relationships` optional-field tolerance

**~5 lines (2 source, 3 test).**

`skills/deliver/scripts/delivery_policy_validation.py:293-294`:

```python
    if "fabric_relationships" not in run:
        return
```

Pinned by `tests/test_delivery_contract.py:560-562`. This is the one item from
the brief's four cited locations that is a genuine D2 target. It is two lines.

### Item 9. Small residue

**~34 lines total.**

| File and lines | What it does | Est. |
|---|---|---|
| `scripts/configure-agent-fabric-mcp.py:153-166` | `agents_home` parameter "remains part of the public call shape while existing callers migrate", then `del agents_home` at `:166`. Plus the vestigial `--agents-home` flag that `scripts/install-harness:133` still passes | 10 |
| `scripts/provenant:26,30,53-57` | `AGENTS_HOME` kept as a third-choice product-root fallback behind `AGENT_FABRIC_PRODUCT_ROOT` and the instance pointer file | 7 |
| `.github/workflows/ci.yml:434-439` | Six-line comment narrating a failure-oracle pin that has already been deleted | 6 |
| `scripts/manage_installation.py:21-27,653-655` | Triple-nested optional import of `agent_installation` ending `= None`, plus an "unavailable" guard. `scripts/agent_installation.py` is present in-tree, so the `None` arm is unreachable | 5 |
| `scripts/public_release_check.py:59-62` | `FORBIDDEN_PREFIXES` guarding against the return of four retired skill directories (`skills/clean-writing/`, `skills/humanise-text/`, `skills/tanstack-query-best-practices/`, `skills/vercel-react-best-practices/`) | 4 |
| `scripts/instance_installation.py:385` | `for name in ("AGENT_FABRIC_INSTANCE_ROOT",):` a single-element loop left from when several env names were accepted | 2 |

### Confirmed absent

Worth recording, because their absence is what keeps the total moderate:

- **No `schema_version` dual-version branches anywhere.** Every occurrence
  across `runtime/agent-fabric*/src`, `scripts/lib/product_root_resolver.py`,
  `scripts/manage_installation.py` and `config/skill-renames.json` hard-pins
  `1` and rejects anything else. There is no `if v1 else v2` shim in the repo.
- **No deprecated model aliases.** The `alias` concept in
  `config/model-routing.json` and `config/model-preferences.json` is role tiers
  (scout, workhorse, flagship), not retired names.
- **No dual parser for authority envelopes.** ADR 0002's cutover was actually
  executed; `LegacyAuthorityInputV1` does not exist.

### Not D2 targets, flagged so a writer lane does not remove them

- `scripts/agent-fabric-mcp:50` and `scripts/agent-fabric-protocol-build:39`:
  "source-tree fallback for development checkouts without a build". A real dual
  reader, but it serves an in-use development mode, not an old format.
- `scripts/change_gates.py:28-55` and `scripts/change_gate_runner.py:15-21`:
  package-versus-script dual import. Both invocation modes are live.
- `scripts/model_route.py` `effort_fallback_order`,
  `runtime/agent-fabric-console/src/protocol-adapter.ts` `#loadWithFallback`,
  `runtime/agent-fabric-herdr/src/degraded-fallback.ts`: runtime degradation,
  not version compatibility.
- `runtime/agent-fabric/src/operator/local-console-session.ts:93-101`: a doc
  comment calling four *currently live* enum members "legacy". Reword at most.

### Total

| Item | Lines |
|---|---:|
| 1. Seat `originKind` cluster | ~626 |
| 2. Skill-rename machinery | ~412 (band 412 to 612) |
| 3. `provenant` legacy-link shim | ~103 |
| 4. Installer doctrine migration | ~65 |
| 5. `change_gates.py` fallbacks | ~60 |
| 6. `ui-ux-design` own legacy layout | ~30 |
| 7. Legacy Git archive media type | ~25 |
| 8. `fabric_relationships` tolerance | ~5 |
| 9. Small residue | ~34 |
| **Total** | **~1,360 to ~1,560, point estimate ~1,450** |

**Honest sizing.** This is larger than the brief's framing ("the known
legacy-link shim") suggested, by more than an order of magnitude, but it is not
a repo-wide reckoning either. Two clusters, items 1 and 2, are 71 per cent of
the total, and both are self-contained. About 60 per cent of the removable
lines are test code, which is normal for a repo of this discipline and means
the risk profile is better than the raw count implies. Roughly 950 of the
~1,450 lines are on code paths that are provably **dead on this machine right
now**, verified against on-disk state rather than assumed.

---

## 4. Cutover plan

One row per removal. `Rewrite` means the test's non-legacy assertions must be
preserved while the legacy scenario is dropped; `Delete` means the whole test
exists only for the legacy path.

| # | What goes | Tests that pin it | Delete or rewrite | Risk |
|---|---|---|---|---|
| 1.1 | `seat-store.ts:69-73,297-309,330-346,348-382` and `:16` optional `originKind`: the whole `legacy-bootstrap.json` marker API. Make `originKind` required in `SeatMetadata` | `tests/unit/seat-store.unit.test.ts:9,21-55` | **Delete** the `reports only the first durable recording of legacy bootstrap provenance` test and the `markLegacyBootstrapSeatGeneration` import | **Medium.** Making `originKind` required is a stored-state contract change. All 27 live seat files already carry it (verified), so no reset is needed here, but see section 5. |
| 1.2 | `mcp-bootstrap.ts:35,659,661-681,698-705`: the `stagedSeats(false)` retry and the `legacy-bootstrap-provenance` ledger push. Keep the `stagedSeats(true)` install and the `SeatGenerationChangedError` arm at `:678-679` | `tests/unit/mcp-bootstrap-legacy-provenance.unit.test.ts` (all 271 lines) | **Delete the whole file** | **Low.** The retry only fires on a daemon rejection of `originKind`, which the current daemon does not produce. |
| 1.3 | `mcp-peer-provision.ts:27,346-349,356-363`: marker fallback in `installedOriginKinds`. Keep the `throw` at `:364-367` and make it the only outcome for absent `originKind` | `tests/system/lifecycle/mcp-zero-state-bootstrap.test.ts:14,252-264,299-305` | **Rewrite**: the two tests at `:214` and `:266` also assert replay and pointer-restore semantics that must survive. Strip only the `readLegacyBootstrapSeatGeneration` assertions. | **Low** |
| 1.4 | `mcp-provision.ts:78,445-446,470-478`: drop `\| null` from the `originKind` maps; make `"originKind" in metadata` an invariant | none directly | n/a | **Low** |
| 1.5 | `credentials.ts:8,256-259,290-293`: `verifiedLegacyBootstrapSeat`. Collapse `(bootstrapSeat \|\| verifiedLegacyBootstrapSeat)` to `bootstrapSeat` | `tests/unit/mcp-credentials.unit.test.ts:10,277-330` | **Delete** the `handles a verified legacy bootstrap seat %s` parametrised test | **Low** |
| 1.6 | `status.ts:399`: `?? "legacy-bootstrap"` | none | n/a | **Low** |
| 1.7 | `lifecycle-receipt.ts:37-42,153-159`: the `"legacy-bootstrap-provenance"` action variant and parser arm | `tests/system/lifecycle/zero-touch-lifecycle-receipt.test.ts:317-350` | **Delete** the `reports first legacy bootstrap provenance exactly once` test | **Low.** Removing a receipt action variant is safe once nothing emits it (1.2 removes the only emitter). Order 1.2 before 1.7. |
| 1.8 | `tests/integration/mcp-proxy-lifecycle.integration.test.ts:620`: client name string `"legacy-bootstrap-contract"` | itself | **Rewrite** (rename the string only) | **None.** Cosmetic. |
| 2.1 | `scripts/manage_installation.py:335-453,531-558,640,650`: `_load_renames`, `_prepare_renames`, `_rollback_renames`, `_apply_renames`, the `reconcile` rename branch and the `--renames` argparse surface | `tests/test_install_skills.py`: 12 functions at `:173,341,685,739,755,792,834,859,883,1112,1145,1197` | **Mixed, UNRESOLVED.** See section 6, Q1. Some `reconcile` tests exercise managed-link drift repair, which is not a rename path and must survive. | **Medium.** Removes a documented operator recovery tool. |
| 2.2 | `config/skill-renames.json` (whole file) | as 2.1 | **Delete** with 2.1 | **Low.** All 12 source names are gone from the repo. |
| 2.3 | `MAINTAINING.md:119-122`: the rename runbook paragraph | none | n/a | **Low** |
| 3.1 | `scripts/install-provenant-command.py:20,90-114,169,174,210,256,264,271,276`: `"legacy-link"` state, `_normalised_link`, and the whole `legacy_target` parameter thread | `tests/test_install_provenant_command.py:36-64,80-104,114-136,155-176,191+` | **Mixed.** Delete the tests whose scenario *is* the legacy symlink (`:36-64`); **rewrite** the publish and exchange tests that merely pass `legacy_target` as a required argument while testing something else | **Low.** Dead on this machine (see section 5), and `classify` retains `absent`, `existing`, `managed-file`. |
| 3.2 | `scripts/install-harness:116,121,156`: `legacy_provenant_target` and both `--legacy-target` arguments | `tests/test_install_harness.py` (indirect) | **Rewrite** if any test asserts on the argv | **Low.** Must land in the same commit as 3.1, since `--legacy-target` is `required=True`. |
| 4.1 | `scripts/install-harness:193-197,200-222,231,237-242`: `legacy_doctrine`, `migrate_instructions()`, the extra symlink arm, and the migration branch | `tests/test_install_harness.py:1185-1198` | **Delete** the migration test; keep the created/existing/preserved tests | **Low.** Dead twice over on this machine (section 5). |
| 5.1 | `scripts/change_gates.py:125-136`: dead inline classifier body. Collapse `classify_failure` to a direct call to the imported implementation | `tests/test_change_gates.py:35-38` helper | **UNRESOLVED**, see section 6, Q2 | **Low** for the code; the test question is the open part. |
| 5.2 | `scripts/change_gates.py:154-173`: dead inline `subprocess.run` body. Collapse `run_command` likewise | none | n/a | **Low** |
| 5.3 | `scripts/change_gates.py:140-141`: `globals().get` in `runner_for_command`; `:685-686`: `globals().get("runner_for_command")` | `tests/test_change_gates.py:311` (dispatch test, unaffected) | keep | **Low** |
| 5.4 | `scripts/change_gates.py:693-699`: `except TypeError` retry of `run_command` without `runner` | none | n/a | **Low.** The current `run_command` signature accepts `runner`; the retry is for a signature that no longer exists. |
| 5.5 | `scripts/change_gates.py:120` docstring: drop "with a partial-revert fallback" | none | n/a | **None** |
| 6.1 | `skills/ui-ux-design/scripts/impeccable-paths.mjs:38-40,49-52,60,65,95`: `getLegacyLiveConfigPath` and `getLegacyLiveServerPath` and their call sites | UNRESOLVED, see section 6, Q3 | UNRESOLVED | **Low.** No `config.json` exists in the skill's scripts directory on this machine. |
| 6.2 | `skills/ui-ux-design/scripts/impeccable-paths.mjs:104-114` and `live-session-store.mjs:3,9,25-26,32,37-39,68`: `.impeccable-live/` dual read and the copy-forward at `:37-39` | UNRESOLVED | UNRESOLVED | **UNRESOLVED**, see section 6, Q4. |
| 6.3 | **Do not remove:** `impeccable-paths.mjs:21-22`, `load-context.mjs:41,63-92`, `live-server.mjs:44-45,199-200` | n/a | **Leave alone** | Subject A. Removing these breaks the skill against real user projects. |
| 7.1 | `skills/deliver/scripts/software_delivery_validation.py:148-156,174-182,199`: the `application/x-git-archive` branch. Reduce the media-type check to `application/x-git-revision` only | `tests/test_software_delivery_continuity.py:126` | **Rewrite**: keep the surrounding continuity assertions, drop the legacy media-type case | **Medium.** Any stored receipt in local state carrying the legacy media type becomes unreadable. Per ADR 0002, regenerate or reset rather than bridge. |
| 7.2 | `skills/release/scripts/validate_release.py:228`: mirror check | `tests/test_release.py` (verify) | **Rewrite** if pinned | **Low.** Must land with 7.1. |
| 7.3 | `skills/deliver/references/contract.md:66`: the historical-format promise sentence | `tests/test_delivery_contract.py` and `tests/test_ci_repository_assurance_policy.py` may assert on contract text; **verify before editing** | **Rewrite** | **Low** |
| 8.1 | `skills/deliver/scripts/delivery_policy_validation.py:293-294`: the `if "fabric_relationships" not in run: return` early exit. Make the field required | `tests/test_delivery_contract.py:560-562` | **Rewrite**: delete the three-line `legacy` block; keep the `candidate` and `coordinated` assertions and rename the test to drop `without_breaking_legacy_receipts` | **Low.** `skills/deliver/templates/RUN.template.json` already emits the field. |
| 9.1 | `scripts/configure-agent-fabric-mcp.py:153-166`: the `agents_home` parameter and `del agents_home`; `scripts/install-harness:133`: the `--agents-home` argument | verify in `tests/test_configure_agent_fabric_mcp.py` | **Rewrite** call sites | **Low.** Value is already discarded. |
| 9.2 | `scripts/provenant:26,30,53-57`: the `AGENTS_HOME` fallback arm | verify in `tests/` for `product_root_resolver` | **Rewrite** | **Low.** `AGENTS_HOME` is unset in the live environment and the instance pointer file exists (section 5). |
| 9.3 | `.github/workflows/ci.yml:434-439`: stale comment | none | n/a | **None** |
| 9.4 | `scripts/manage_installation.py:21-27,653-655`: unreachable optional-import `None` arm | verify | **Rewrite** if pinned | **Low** |
| 9.5 | `scripts/public_release_check.py:59-62`: four `FORBIDDEN_PREFIXES` for retired skills | `tests/` for `public_release_check` | **Rewrite** | **Low.** These guard against resurrection of deleted directories; arguably cheap insurance. Removal is optional. |
| 9.6 | `scripts/instance_installation.py:385`: single-element loop | none | n/a | **None** |

**Suggested lane ordering.** Item 1 is one lane (order 1.2 before 1.7). Item 2
is a second lane and should not start until Q1 is answered. Items 3 and 4 are a
third lane, both installer, both dead here. Item 5 is a fourth lane and should
not start until Q2 is answered. Items 7, 8 and 9 are a small fifth lane. Item 6
waits on Q3 and Q4.

---

## 5. Install and upgrade constraint

The brief asks whether any removal would break this machine's ability to
install or upgrade the harness as it currently stands. **Every install-touching
removal was checked against live on-disk state. None of them breaks it.**

| Path | Live state checked | Finding |
|---|---|---|
| `provenant` legacy-link (item 3) | `$HOME/.local/bin/provenant` | A **regular file**, mode `-rwxr-xr-x`, 4115 bytes, not a symlink. `classify` cannot return `legacy-link` here. **Safe to remove.** |
| Installer doctrine migration (item 4) | `$HOME/.claude/CLAUDE.md` | Contains **neither** `.agents/AGENTS.md` nor `.agents/HARNESS.md` as a literal string (zero grep matches). The guards at `install-harness:231,235,237` all fail today; control already reaches the `else` at `:243`. Additionally the layout is fused, so the awk rewrite would be a no-op even if reached. **Safe to remove.** |
| Seat `originKind` becoming required (item 1) | 27 seat metadata files under `$HOME/.local/state/agent-harness/fabric/seats/*/generations/<current>/` | **All 27 carry `originKind`.** No `legacy-bootstrap.json` marker exists anywhere in the state tree. **Safe to remove.** |
| `AGENTS_HOME` product-root fallback (item 9.2) | environment and `$HOME/.agents/.agent-fabric/product-root.json` | `AGENTS_HOME` is **unset**; `AGENT_FABRIC_PRODUCT_ROOT` and `AGENT_FABRIC_INSTANCE_ROOT` are also unset; the instance pointer file **exists** (61 bytes, dated 2026-08-02). Resolution takes the pointer arm. **Safe to remove.** |
| `.impeccable-live/` dual read (item 6.2) | `$HOME` to depth 3 | **No `.impeccable-live` directory exists.** Safe on this machine, but see Q4 for the general case. |
| Skill-rename machinery (item 2) | invocation graph | Not called by `install-harness`, `install-skills` or CI. Only a hand-run per `MAINTAINING.md:119-122`. **Removing it does not affect install or upgrade**; it removes an operator recovery tool. |

**Three genuine cautions, none of them blockers:**

1. **Item 1 and item 7 change stored-state contracts, not just code.** Making
   `originKind` required, and dropping the `x-git-archive` reader, means any
   *future* state written by an older build becomes unreadable. That is
   precisely what ADR 0002 authorises ("regenerated or the local pre-release
   state is reset"), but the writer lane should not silently assume it; if any
   archived receipt under `$HOME/.local/state/agent-harness/fabric/archives/`
   or `.../backups/` is ever intended to be re-read, it must be regenerated
   first, not bridged.

2. **Item 3.1 and 3.2 must land in the same commit.**
   `install-provenant-command.py:256` declares `--legacy-target` with
   `required=True`. Removing the flag without removing the caller, or the
   caller without the flag, breaks `install-harness` at the preflight step
   (`scripts/install-harness:117-122`), which exits 3 before any mutation.

3. **A pre-existing, unrelated condition worth reporting.** By reading the
   guards at `scripts/install-harness:224-247` against the live
   `$HOME/.claude/CLAUDE.md`, `scripts/install-harness` appears to reach the
   `else` arm and `exit 3` with "instructions preserved" **today**, before any
   cutover. This is not caused by D2 work and is not made worse by it, but it
   means "the installer currently succeeds end to end on this machine" should
   not be assumed as a baseline by whoever verifies the cutover. This inference
   is from reading the guards, not from running the installer, which would
   mutate state.

---

## 6. UNRESOLVED items

Four. Each is a specific question, not a guess.

**Q1 (item 2, largest uncertainty).** `tests/test_install_skills.py` has 12
functions matching `rename|reconcile`, but `reconcile` is also the name of the
ordinary managed-link drift repair action, which is **not** a rename path and
must survive. Which of the 12 are rename-only? Specifically: are
`test_installer_reconciles_previously_managed_link_drift` (`:341`) and
`test_reconcile_repairs_broken_managed_link_and_rejects_conflict` (`:685`)
reachable without `--renames`, and if so, does removing the rename machinery
leave the `reconcile` action itself intact and worth keeping? This determines
whether item 2 is ~412 or ~612 lines, and whether `reconcile` survives at all.

**Q2 (item 5).** `tests/test_change_gates.py:35-38,41-54,57-60` contain
assertions worded for a reverted implementation ("structured classifier is
absent at this revert point", "change-gate implementation is absent at the
merge base"). The change-gate machinery reverse-applies hunks and re-runs tests
against the repo's own source, so these tolerances may be **load-bearing for the
gate self-hosting on its own diff**, in which case they are correct engineering
and not legacy residue. Question: does `scripts/check-test-gates` ever
reverse-apply hunks in `scripts/change_gates.py` itself such that
`_structured_classify_failure` becomes unbound at test time? If yes, keep the
test helpers and remove only the production fallbacks (5.1 to 5.4 are still
safe, since the import binds unconditionally). If no, delete the helpers too.

**Q3 (item 6.1).** Are there tests covering
`skills/ui-ux-design/scripts/impeccable-paths.mjs`? No test file was located for
it. If the skill's scripts are untested, removal is unverifiable by the suite
and needs a manual smoke of `skills/ui-ux-design/scripts/live.mjs` against a
scratch project instead.

**Q4 (item 6.2).** Is `.impeccable-live/` this repo's own state format or the
user's project state? It is written under the *target project's* `cwd`, not
under `$AGENTS_HOME`, which argues subject A; but it is a directory only this
harness's tooling ever creates or reads, with no external consumer, which
argues subject B and therefore D2. The copy-forward migration at
`live-session-store.mjs:37-39` is a genuine migration path either way. Question
for the user or chair: does D2's "this repo's own code" extend to state formats
this repo's tooling writes into other people's directories? None exist on this
machine today, so the practical cost of either answer is zero right now.

---

## 7. Method note

Read-only throughout. No test suite was run in full. Claims about reachability
were checked three ways: by reading the guard, by importing the module and
inspecting bound names, and by inspecting live on-disk state under `$HOME`. The
`change_gates` fallback finding and the four items in section 5 were verified
directly rather than inferred. The inventory combines a targeted read of the
four cited locations and their artifacts with a repository-wide marker sweep;
the two passes were reconciled and each pass caught clusters the other missed.
