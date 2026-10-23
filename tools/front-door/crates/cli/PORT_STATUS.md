# CLI port batch 3

Author route: `codex/gpt-6.1-sol@high`. Rulebook: `PORTING.md` version 4.
Source/oracle revision: `1884954ef414e60cc6b160c636fe1add9f7784dd`;
merged main: `8bf118d933ed835add47e22c9c562beaf6747efc`. Assigned Python sources
are unchanged between those revisions. No command owner switches and no Python
or TypeScript source is removed. This is an inert, partial library delivery.

## Fully ported

- `scripts/check-provenant-install.py#interface`: arguments, actual six-provider
  state, ordered installation failure/status contracts and output boundary.
  `render_bytes` preserves arbitrary Unix path bytes, embedded newlines and
  advisory bytes. `render` is a Unicode convenience renderer; executable
  integration must use `render_bytes`. Config, pointer, digest and provider
  inspection are implementation-owner integrations outside this interface.

The already completed `scripts/lane_safety_guard.py#unit-01` receives the #1326
repair: `kill_reason` returns `Option<String>` with no vestigial IntegerError.
It is not counted again in the batch's `Units:` line or `ported.txt` append.

## Partial coverage and exact remaining work

| Assigned unit | Implemented now or retained | Remaining / named prerequisite |
| --- | --- | --- |
| `scripts/check_harness.py#unit-01` | Retained decoded sidecar policy and diagnostics | YAML parser with PyYAML value/error semantics; Markdown anchors/links, decoded issue forms and route/skill validation. No allowlisted YAML reader/API exists on main. |
| `scripts/clean.py#interface` | Retained arguments, approvals/plans/context/removal records and renderers | Records owner Python filesystem-path JSON codec (lone surrogates); full JSON/text rendering and planner binding. Current record strings cannot represent surrogateescaped paths. |
| `scripts/git_evidence.py#unit-01` | Closed environments, literal selection/untracked policy; typed metadata and exact ASCII compact header/status framing; raw-byte GitRead seam replaces the local unbounded child helper | Supervisor byte-preserving bounded executor and streaming sink with cleanup; filter discovery, tracked snapshots, incremental UTF-8 diff decoding, capture/change detection, safe atomic publication and main. |
| `scripts/lib/stale_checkout.py#interface` | Retained local-query/environment/deadline contract, snapshots and advisory suppression | Supervisor exported strict text runner (public bounded runner replacement-decodes); core Python OS-string/path text boundary. |
| `scripts/lint-timebomb-fixtures.py#unit-01` | New complete textual JS/TS date and changed-test-context policy | Agreed Python AST parser for Python windows/fixtures/helpers; Git changed-line collection, traversal, diagnostic/main binding. |
| `scripts/provenant_pr.py#unit-01` | Existing judge/settlement/exit policy; new checked, ordered check-response normalization and required/all fallback classifications | Discovery of rulesets/classic protection; gh launch/view, polling and moving-head orchestration, backoff/env parsing, rendering and main. Full raw Python JSON semantics (surrogates/non-finite values) remain a records prerequisite. |
| `scripts/reap.py#unit-01` | New literal PID validation, dead-state policy, descendant ordering, age/description formatting, protected ancestor walk and victim/refusal selection | Supervisor per-PID TERM/KILL authority and cross-platform CWD observation; local-time parser; liveness/grace/reuse/reap effect integration, argv and main. No process is signalled by this crate. |
| `scripts/retry-npm-install.py#unit-01` | Existing retry signatures/tail/budget/delays; new exact argv separator and missing-command contract | Supervisor concurrent live stdout/stderr forwarding and joined-reader custody, launch/wait and clock-driven retry driver. Public file-backed bounded runner is not a live-output adapter. |
| `scripts/worktree.py#interface` | New actual create/list/check/context/verify/remove arguments; actual receipt field shapes; lossless ordered porcelain OS-value types | Source surrogateescaped path JSON codec; full effect/error owner contracts. String receipt paths remain partial and must not be used for cut-over. |
| `scripts/check_harness.py#unit-02` | New source-ordered input discovery, checker trait orchestration, output/exit rendering | Concrete unit-01 checker implementations, Python OS-path ordering/text support and executable exception boundary. |
| `scripts/lane_safety_guard.py#unit-02` | Existing recursive decisions; new typed hook envelope, exact ensure_ascii decision output, checked decoded event boundary | Raw syntax-error compatibility, escaped lone-surrogate inputs and process main, as RP-CLI2-03 already records. Decode errors stay explicit; this is not a complete raw hook entry point. |
| `scripts/lib/stale_checkout.py#unit-01` | Retained local observation collection and once-per-tree warning policy | Same strict text/OS-path owner APIs as interface; numeric/clock edge behavior and executable environment forwarding. |
| `scripts/public_release_check.py#unit-01` | New ordered registry, exact byte classification/Git grep prefilter, required/forbidden paths, safe-handle current-tree reads, real tree tuple shape | Supervisor bounded raw Git evidence with stdin for batch-check, tree/object/history enumeration and exact decoders; complete streaming text scan for files growing after stat (RP-CLI3-03). |
| `scripts/worktree.py#unit-01` | New ordinary/hardened closed Git environment table and filter-setting policy | Supervisor raw dual-stream capture with independent limits, unlimited porcelain stdout, full byte counts/digests and surrogateescape; diagnostic redaction/capture, owning-root/back-pointer verification. |
| `scripts/public_release_check.py#unit-02` | New publication tree policy, commit/tag record interpretation and personal-email classification | Raw Git grep/findings execution, history/range orchestration, main; depends on unfinished unit-01 evidence APIs. |
| `scripts/worktree.py#unit-02` | New complete pure porcelain decoder, safe-name/full-OID checks, generated ignored-path/residue rules and pre-observation claim refusals | Safe raw Git binding, primary/common-root observation, COW provisioning/rollback, full claim checks and protected-instruction gate. |
| `scripts/worktree.py#unit-03` | New create-name derivation and warning/refusal policy; arguments above | Root/exclude/create/list/check/context effects and writer lease/removal lifecycle. Safe FS and core excludes now exist; Git capture, protected-instruction gate and clean integration-ref/tip policy are not complete. |

## Skipped units after prerequisite audit

- `scripts/movecheck.py#unit-01`: needs Python AST parsing and canonical `ast.dump`
  equivalence. Comparing raw Rust token strings would not preserve the source.
- `scripts/static-security-check.py#unit-01` and `#unit-02`: need the same agreed
  Python AST representation, lexical scopes/bindings and branch-sensitive drain
  analysis. No allowlisted parser/API is available on main. No regex substitute
  or Python production invocation is introduced.

## Shared owners and pending work

CLI directly adopts `provenant_core::py` (existing shared semantics),
`provenant_records::checked_json` (all new raw JSON integer validation), and
`provenant_supervisor::fs::SafeFs` (publication content reads). The supervisor's
public bounded runner returns replacement-decoded strings; it cannot stand in
for the strict/raw/streaming contracts above. The old CLI unbounded
`Command::output` Git implementation is deleted rather than kept as a second
process owner. GitRead is an injectable CLI evidence seam with no concrete
lifecycle implementation.

Remaining `pending` items, each owner-marked:

- `provenant-core`: decimal_digit/python_digit, IntegerError/decimal_integer,
  python_splitlines, python_word_character and the Unicode alphanumeric table.
  The integration owner was notified through Fabric. Main still has no public
  equivalents, so these copies cannot yet be deleted.
- `provenant-records`: minimal ensure_ascii JSON output adapters and ordered
  JSON value storage/repr. OrderedValue validates through checked_json first;
  its ordered traversal aligns scalars with that checked result, including
  integer -0 and exact i64/u64 values. Duplicate keys update in place. It does
  not duplicate integer validation or timestamp conversion. Records has private
  encoders, but no public equivalent to these adapters yet.

## Evidence and review

Local Rust toolchain: pinned 1.94.1. Required targeted commands: cargo fmt,
crate clippy with warnings denied, required crate differential tests. Test/oracle
revisions are the source revision above plus this PR's Rust tests. The local
repository virtual environment currently resolves to CPython 3.15.0; CI's
Python 3.14 remains a separate check. No claim is made that every Unicode
codepoint has been revalidated against the newer interpreter.

Differentials use the single existing oracle helper: PROVENANT_PORT_PYTHON,
-I, an explicit environment, bounded file-backed output/waits, skip diagnostics
and mandatory-oracle failure. New cases cover byte/Unicode token boundaries,
email case folding, required/symlink/oversize publication precedence, actual
commit/tag Git records, actual primary/linked/bare porcelain (spaces/newlines;
invalid-byte directory names on Linux, which macOS cannot create), malformed
porcelain native policy fixtures, hardened filter discovery, exact packet bytes,
ordered duplicate-key PR checks and negative zero, decoded hook events and
non-UTF-8 install path output. Fixtures use explicit clocks and no sleep
assertions. Git fixture setup/discovery uses closed environments.

Native read-only source review assumed the port was wrong and found a potential
silent truncated publication scan after stat/open growth. The repair returns an
explicit typed ChangedDuringRead refusal before UTF-8 decoding; an injected
safe filesystem fixture demonstrates the condition without a timing race.
RP-CLI3-03 keeps its source difference open and this unit partial. Raw hook
syntax/surrogate gaps remain explicit and tested under RP-CLI2-03.
Cross-family review, exact-head PR CI and J1/J2/J3 remain the chair's landing or
cut-over gates; this lane does not claim them or arm auto-merge.

Final local result: 67 crate tests passed, 0 failed; warnings-denied clippy and
cargo fmt passed. The targeted layering tests also passed (2 tests). The
native source reviewer and repair-delta reviewer reported no remaining finding
after RP-CLI3-03 was documented. No cross-family review is claimed.
