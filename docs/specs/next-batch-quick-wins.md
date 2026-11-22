# Next batch: quick wins

Owner: the chair running this batch
Anchor: `docs/specs/next-batch/quick-wins.toml`, `docs/specs/next-batch/briefs/`

These items touch no shared seam and can land now, before the batch, as one
`batch` run into `batch/quick-wins` and one pull request. Each item is
independent: one that fails review or conflicts is dropped from the run,
without blocking the rest.

All use Hy (`opencode/opencode-go/hy4-flash-free`) except where noted. Lanes run `cargo fmt` only. Each
item adds a fragment, `docs/release-notes/next.d/<issue>-<slug>.md`. This page
is transient, like [the design](next-batch.md).

Launch: from the root of the checkout that holds these files, run
`provenant run --file docs/specs/next-batch/quick-wins.toml --explain`, then
launch the same command without `--explain`. Create the repair checkout
first with `provenant worktree create batch/quick-wins-repair --base main`.

| Item | Issues | Lines | Files |
|---|---|---|---|
| qw-check-install-roots | #3811 | 200 | `tools/front-door/src/checks.rs`, `tools/front-door/crates/config/src/managed.rs`, `tools/front-door/crates/cli/src/check/mod.rs` |
| qw-tier-command-string | #3812 | 200 | `tools/front-door/crates/config/src/ports.rs` |
| qw-install-upkeep | #3799 #3813 #3812 | 350 | `tools/front-door/src/install.rs`, `tools/front-door/src/release_smoke.rs`, `tools/front-door/src/tick.rs` |
| qw-output-null-headers | #3807 | 120 | `tools/front-door/crates/cli/src/run/output.rs` |
| qw-opencode-archived | #3710 | 80 | `tools/front-door/crates/providers/src/opencode.rs` |
| qw-help-and-grammar | #3808 #3814 | 350 | `tools/front-door/crates/cli/src/lib.rs`, `tools/front-door/crates/cli/src/grammar.rs`, `tools/front-door/src/lib.rs` |
| qw-output-running-partial | #3809 | 200 | `tools/front-door/crates/run/src/reader.rs`, `tools/front-door/crates/cli/src/run/output.rs` |
| qw-budget-completeness | #3405 | 150 | `tools/front-door/crates/cli/src/run/holder.rs` |
| qw-lane-reads-chair-graphs | #3192 | 350 | `tools/front-door/crates/cli/src/fabric/graph_relay.rs`, `tools/front-door/crates/fabric-runtime/src/coordination/facade.rs`, `tools/front-door/src/fabric/lane_read_tests.rs` |
| qw-smoke-retry-steer | #3502 | 200 | `tools/front-door/crates/fabric-runtime/assets/templates/smoke.toml`, `tools/front-door/src/install.rs` |
| qw-gc-default-branch | #3768 | 250 | `tools/front-door/src/lane_cleanup.rs`, `tools/front-door/crates/confinement/src/worktrees/gc.rs` |
| qw-pinned-redraw-fidelity | #3764 | 300 | `tools/front-door/crates/cli/src/run/provider/preparation.rs`, `tools/front-door/crates/routing/src/` |
| qw-docs | #3784 #3814 | 300 | `skills/orchestrate/SKILL.md`, `docs/quickstart.md`, `docs/README.md`, `README.md`, `docs/runbooks/install.md` |

## qw-check-install-roots: check install verifies the recorded install roots, whatever HOME is

Issues: #3811. Order: after #3816 merges (it edits `src/checks.rs`).

- Outcome: `provenant check install` builds its targets from the install receipt's recorded roots, not from the caller's `$HOME`. With `HOME` set to an account directory (as inside a lane), a healthy install reports 0 findings.
- Acceptance: `Targets::new`, or its replacement, takes the recorded roots from the receipt, and no path in the check reads `HOME` directly.
- Test: A check test with a fixture receipt and a context whose home differs from the recorded root reports no `missing_receipt`. Pass the home through the context; never mutate the process environment.

## qw-tier-command-string: ports.json accepts a string command as TierCommand shorthand

Issues: #3812.

- Outcome: In `verify.tiers[].commands[]` a plain string such as `"cargo fmt {packages} -- --check"` decodes as a `TierCommand`: argv split with shell-word rules, no shell, and `stack` and `cwd` unset. The pre-decode filter accepts it.
- Acceptance: The string and the table forms of the same command produce equal `TierCommand` values. A string that does not split, such as an unbalanced quote, is a malformed-declaration warning naming the leaf, as today.
- Test: A decode test of both forms; a malformed-string test.

## qw-install-upkeep: upgrade --verify runs the new binary; retire cargo-target-seeds; smoke leaves no registered scratch project

Issues: #3799 #3813 #3812.

- Outcome: (1) After an upgrade installs, the verification runs the installed destination binary (exec with `upgrade --verify`), not the running pre-upgrade process. (2) Install removes `<state>/cargo-target-seeds/` when present, printing its path and size; it belongs to the seeding feature that #3790 removed. (3) The smoke fixture unregisters the scratch project it registers, so `tick` no longer reads its `ports.json`. Check (3) on v2.8.2 first: if #3800 or #3815 already removed the leftover registration, drop it from the slice.
- Acceptance: The verify program path equals the installed destination. A state root with `cargo-target-seeds/` loses it once and reports it. After `upgrade --verify`, the host registry lists no project under the scratch fixture root.
- Test: Inject the runner and assert its program path; run a state-root fixture with and without the directory; assert the registry after the smoke fixture.

## qw-output-null-headers: output omits header lines whose value is null

Issues: #3807.

- Outcome: A header key that is present with a JSON `null` value is treated as absent, so no "unavailable" line prints.
- Acceptance: Headers print only for non-null values.
- Test: Render output from a record with null `branch` and `commit` keys; the lines are absent. Assert absence by key, not by full text.

## qw-opencode-archived: opencode lanes pass --archived

Issues: #3710.

- Outcome: Lane launches pass `--archived`, so lane sessions stay out of the owner's OpenCode session lists.
- Acceptance: The launch argv contains `--archived` for every lane role.
- Test: An argv test over the launch builder.

## qw-help-and-grammar: Nested help paths, globals once in help --all, config path [DIR], stale doc strings

Issues: #3808 #3814. Order: after #3817 merges (it edits `crates/cli/src/lib.rs`).

- Outcome: (1) `provenant help run implement` prints that subcommand's help; an unknown path lists the valid children at the deepest matched level. (2) `help --all` prints the global options once, at the top. (3) `config path [DIR]` takes an optional positional directory, and the ignored required `--project` bool on it is deleted with no alias. (4) The `pr` doc string no longer claims a watch command, and `worktree`'s `--base` doc string matches what the flag does.
- Acceptance: All four hold in `provenant help --all` and `provenant config path .`.
- Test: A help-tree test: nested resolution, and each global option name appears once in `help --all`. Count names, never snapshot prose. A grammar test for the positional.

## qw-output-running-partial: output on a running attempt prints the partial result

Issues: #3809.

- Outcome: `provenant output ATTEMPT` on a running attempt prints what has arrived, under a `running` header, and exits 0. `RunError::Nonterminal` is deleted from this path.
- Acceptance: A running attempt's output exits 0, carries `status: running` in the envelope and states that the result is partial.
- Test: A reader test with a nonterminal attempt fixture.

## qw-budget-completeness: Token-budget settlement keeps the decoder's completeness

Issues: #3405.

- Outcome: The budget path no longer sets `decoded.complete = true`. One gate decides success, and a truncated, non-empty result under a done handoff does not settle as `Succeeded`.
- Acceptance: A budget-stopped lane with a truncated result and a done handoff settles as incomplete.
- Test: A holder settlement test for that case, plus the empty-result case keeping its handoff-fence substitution.

## qw-lane-reads-chair-graphs: A lane may read any graph its own chair session launched

Issues: #3192.

- Outcome: The lane read path allows status, output and trace for any graph whose chair session equals the lane's own. Writes (steer, stop, retry) stay limited to its own graph. If the graph record has no chair session field, add one, set at submission.
- Acceptance: The lane read of a sibling graph from the same chair session succeeds; a graph from another session stays `not_found`.
- Test: Flip the two denial tests in `src/fabric/lane_read_tests.rs` to the new rule and add the cross-session denial.

## qw-smoke-retry-steer: Smoke graph gains retry and steer nodes

Issues: #3502.

- Outcome: `smoke.toml` gains a retry node and a steer node on the cheapest routes, overridable like the others. `upgrade --verify --dry-run` lists them, and a failing one is reported by name.
- Acceptance: The dry-run lists the new nodes.
- Test: Extend the existing smoke dry-run listing test by node id.

## qw-gc-default-branch: gc protects the default branch on remote refs and linked worktrees

Issues: #3768.

- Outcome: `remote_outcomes` applies the same default-branch exclusion as local retirement when `target` is `None`: in dry-run, and when `remote_default` fails. A linked worktree checked out on the default branch is never retired. Excluded branches show as Retained with a reason.
- Acceptance: No gc path retires or modifies the default branch.
- Test: Remote-ledger fixtures for dry-run and a failed `remote_default`; a linked-worktree-on-default fixture.

## qw-pinned-redraw-fidelity: Pinned exhausted route: advisory attempt, true cause, future expiry

Issues: #3764. Uses DeepSeek: routing judgement. Lands before w3-accounts, which edits the same file.

- Outcome: F1: when the queue drains after an advisory redraw, the pinned route is attempted once on its refused account, with the warning. F2: a provider-refusal redraw records its real exclusion reason, not Exhausted. F3: an advisory expiry is used only when it is in the future.
- Acceptance: As in the issue's rescope comment.
- Test: One test per finding at the preparation boundary.

## qw-docs: Chair-rule home and a one-page quickstart

Issues: #3784 #3814.

- Outcome: (1) The one-accountable-chair rule gets its home in `skills/orchestrate`. (2) `docs/quickstart.md` gives one runnable first-install sequence: install, `provenant check install`, register an account, a first read-only lane, a first writer. `docs/README.md` and `README.md` link it as "start here", and `install.md` leads with first install, with upgrade material after it.
- Acceptance: Every `provenant` command in the new text exists; `scripts/check-doc-truth --binary "$(command -v provenant)"` reports no error for these files.
- Test: The doc-truth check itself.
