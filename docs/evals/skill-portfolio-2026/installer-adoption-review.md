# Independent review: installer adoption repair

**Verdict: MERGE-AFTER-FIX.** The adoption logic is correct and the destructive-action
safety is sound, but the change introduces two fresh plan/install/check disagreements of
exactly the class it set out to remove, one of which fails the whole harness install for
any user who keeps a personal agent definition in the client agents directory.

- Target: worktree `installer-adoption-repair`, head `c6d8516e`, commits `74c110ab` and
  `c6d8516e` on baseline `8167c69f`.
- Reviewer did not author the change. All line numbers refer to the worktree copies.
- `$AGENTS_HOME` stands for the product checkout root throughout.

## What the change gets right

The stated repair works. The three real pre-existing files in the client agents directory
are byte-identical to the product sources, so `install-agents` now adopts them and exits
0 where it previously aborted. I reproduced this against copies of the actual files.

Destructive-action safety holds. Every mutation site was traced:

| Site | Guard | Verdict |
|---|---|---|
| `_replace_link` at `scripts/agent_installation.py:264` | state in `{missing, stale, adoptable}` | safe; `adoptable` requires a proven sha256 match |
| retirement `unlink` at `scripts/agent_installation.py:271` | `is_symlink()` only, and pre-checked by `_raise_on_conflicts` | safe |
| `uninstall_managed` `unlink` at `scripts/agent_installation.py:371` | `is_symlink()` only, and pre-checked against the recorded source | safe |
| `_write_manifest` at `scripts/agent_installation.py:149` | mkstemp plus `os.replace` beside the target | safe |

`_same_file_content` (`scripts/agent_installation.py:120-126`) rejects symlinks, rejects
anything that is not a regular file, and only then compares full-byte digests. There is
no path in this diff by which a user's differing file, directory, dangling symlink or
foreign symlink is unlinked, truncated or replaced. I confirmed each of those four
shapes refuses with a remedy message and leaves the path untouched.

The change also correctly declines to add hostile-user hardening. There are no
`O_NOFOLLOW` ladders, no inode re-checks, no same-UID assertions, no TOCTOU retries.
That is the right call for a single-user machine and it should stay that way.

## MAJOR findings

### MAJOR-1: a user's own agent file now fails the harness install

`scripts/agent_installation.py:306-310` adds a scan that labels every `.md` in the target
directory that is neither a product source nor a manifest entry as `foreign`.
`check()` at `scripts/agent_installation.py:319-329` then treats `foreign` as a failure,
so `run` returns 3.

`scripts/install-agents:37` ends with `exec ... check --summary`, and
`scripts/install-harness:180` runs `install-agents` under `set -euo pipefail`. A single
personal definition in the client agents directory therefore fails the entire harness
install, late, after skills and workflows have already been published.

Reproduced end to end:

```
$ install-agents --target <scratch>/agents      # scratch contains my-own.md
agents linked=3 existing=0 target=<scratch>/agents
conflicting: agent installation integrity failed: my-own.md=foreign
install-agents rc=3
```

`plan` returns 0 and `install` returns 0 for the same tree. That is a plan-passes,
check-fails split, the same defect class being repaired.

This also re-diverges from the surface it is meant to converge with. The skills installer
runs the same scan at `scripts/manage_installation.py:283-297`, but tags extras with
`scope: "extra"` and reports them at `scripts/manage_installation.py:676-679` as a
`warning:` line with exit 0. Failures are computed only over `scope == "required"`
(`scripts/manage_installation.py:700-706`). The agents surface now has the opposite
contract under the same CLI flag.

Remedy: drop the scan, or emit the extras as a warning with exit 0 and give the items a
`scope` field so the two surfaces agree. Note that the client agents directory is a
shared user directory by design, so a hard failure there will recur.

### MAJOR-2: check passes and install fails for a managed name replaced by an identical file

`_state` (`scripts/agent_installation.py:169-189`) classifies a managed entry whose
destination is a real file rather than the expected link as `conflicting`, regardless of
content. `_integrity_state` (`scripts/agent_installation.py:201-208`) classifies the same
path as `adoptable`, and `check` whitelists `adoptable` at
`scripts/agent_installation.py:320`.

Reproduced:

```
$ install ...            rc=0
$ rm agy-reviewer.md && cp <source>/agy-reviewer.md agy-reviewer.md
$ check --summary        rc=0   agents checked=3
$ plan  --summary        rc=3   conflicting: conflicting managed targets: agy-reviewer.md
$ install --summary      rc=3   conflicting: conflicting managed targets: agy-reviewer.md
```

`install-harness:87` runs `plan --surface agents` before anything else, so this state is a
hard install stop that `check` reports as healthy. Two further problems compound it:

1. The message from `_raise_on_conflicts` at `scripts/agent_installation.py:234-235`
   carries no remedy at all, unlike the unmanaged branch three lines above. The change's
   own contract is "name the file and the remedy". This branch names the file only.
2. The state is self-inflicted: byte-identical content is exactly the condition the change
   defines as adoptable everywhere else. `_state` should treat it as `stale` and relink.

Remedy: make `_state` consult `_same_file_content` for the managed-entry case and return
`stale`, and give the `conflicting` branch a remedy sentence.

### MAJOR-3: a link that is already canonical is refused, and the remedy tells the user to delete it

`_same_file_content` returns `False` for any symlink
(`scripts/agent_installation.py:121`), so `_state` at
`scripts/agent_installation.py:177` cannot adopt a destination that is already a symlink
pointing exactly at the canonical source when the manifest has no entry for it. The
manifest is machine-local and gitignored, so losing it while the links survive is an
ordinary occurrence.

Reproduced with three correct links and no manifest:

```
plan  rc=3  unmanaged agent targets would be overwritten: agy-reviewer.md: manually
            move or remove <target>/agy-reviewer.md before rerunning to adopt
            <source>/agy-reviewer.md
check rc=3  (same, per file)
install rc=3
```

The link is already exactly what the installer wants to create, and the remedy instructs
the user to destroy it. The classification is inherited from the baseline, but the remedy
text is new in this diff and makes the behaviour actively misleading. This is the
smallest of the three MAJORs; it is listed here because the fix is one clause
(`_same_link(destination, source)` before the content check) and this commit is the one
that owns adoption.

## MINOR findings

### MINOR-1: `docs/specs/harness/lifecycle.md:363-366` describes behaviour the skills installer does not have

The new sentence "An unmanaged file with bytes identical to the product source may be
adopted as a managed link" sits inside the paragraph that describes the skill manifest
("hashes full skill-tree bytes and executable modes"). The skills installer does not
adopt: `scripts/manage_installation.py:165-171` still returns `unmanaged` for any
existing path and never compares content. The spec now over-claims for the surface the
paragraph is about. Scope the sentence to the agents surface.

The preceding edit, "Different unmanaged existing paths are never claimed or
overwritten", is also ungrammatical. "Unmanaged paths whose content differs from the
product source" says what is meant.

### MINOR-2: `docs/adr/0019-installed-file-class-ownership.md:57-65` edits a table the ADR declares frozen

The sentence immediately below the table reads "The table above is the approved decision
and does not change." This commit changed two of its rows. Either the sentence goes, or
the additions belong in the amendment prose beneath it, which is where the ADR already
records "further artifacts are members of classes it already names". As written the
document contradicts itself in adjacent lines.

### MINOR-3: `scripts/manage_installation.py:20-31` leaves an unreachable crash path

Removing the `if agent_installation is None` guard from `main` was right only if the
`None` fallback also went. It did not. If both imports were ever to fail with matching
module names, `main` would raise `AttributeError` while evaluating
`except (OSError, agent_installation.InstallError)`, producing a traceback rather than
exit 3. Delete the `agent_installation = None` branch and let the import fail loudly.

### MINOR-4: `scripts/agent_installation.py:40` drops the empty-source guard

`_sources` no longer rejects a source directory containing no definitions. A mis-set
`--source` or a wrong product root now silently retires every managed link and reports
success. The retirement itself is safe (symlinks only), but the guard was cheap insurance
against operator error, not backwards compatibility. This is deliberate and tested, so it
is recorded rather than blocked.

### MINOR-5: `scripts/agent_installation.py:414-426` recomputes the whole plan three times per install

The install path calls `plan` before, `install`, then `plan` again, plus two extra
`_load_manifest` calls, so `_sources` runs five times and every source file is hashed
repeatedly. Negligible for three agents; it becomes the dominant cost once the
surface-parameterised collapse in issue #14 puts forty-odd skills through the same code.
Compute the `changed` set inside `install` and return it.

### MINOR-6: the agents JSON envelope diverges from the skills envelope

`scripts/agent_installation.py:453-458` emits check items as `{name, state}` with no
top-level `ok`. The skills surface emits `ok` at `scripts/manage_installation.py:532` and
items carrying `owner`, `scope` and `source_target` at
`scripts/manage_installation.py:314-321`. Two shapes now ship under one `--surface` flag,
and the skills post-processing in `main` keys off fields the agents items do not have.
This makes the planned collapse harder, not easier.

Relatedly, `_raise_on_conflicts` now exists in both modules with different signatures and
different semantics. Rename one before they are merged.

### MINOR-7: the gate count does not match the claim

The author reports 60 passed. `tests/test_install_agents.py` plus
`tests/test_install_harness.py` is 58 passed on this head. Not a defect, but the recorded
evidence should match what reruns.

## NIT findings

- `scripts/agent_installation.py:109-117`: `_canonical_directory_link` is named as a
  predicate and returns a bool on one path, raises on another and returns `False` on a
  third. Callers must know which. Split it, or name it for what it does.
- `scripts/agent_installation.py:451`: `check --summary` prints
  `agents checked=<len(sources)>` even when the directory-link exemption checked nothing,
  and prints 3 when four items were classified. Print `len(items)`.
- `scripts/agent_installation.py:377`: `uninstall_managed` returns a three-tuple whose
  second and third elements are permanently `0` and `[]`, and the caller discards both.
  Return the count.

## Over-engineering assessment

Against the repo doctrine, the diff is well behaved. No TOCTOU ladders, no `O_NOFOLLOW`,
no ownership or UID checks, no retry loops. The `_replace_link` restructure at
`scripts/agent_installation.py:129-146` only ensures the staged path is not unlinked after
a successful `os.replace`, which is correctness, not hardening, and it costs four lines.

The one item that does read as hostile-user defence is the foreign scan behind MAJOR-1,
and its test at `tests/test_install_agents.py:268-279` states the framing plainly:
`evil.md` and `pw.md -> /etc/passwd`. It defends nothing. The harness never reads those
files; the client does, and it will read them whether or not `check` exits 3. The scan
buys no safety and costs a false install failure, so it does not earn its complexity
here. Either delete it, or keep it strictly as the warning the skills surface already
emits.

## Test quality

I found no vacuous tests. None assert on a value the same test computed, none mock the
behaviour under assertion, and every new or changed test is capable of failing.

Proven by execution rather than argued: I built a scratch copy of the tree, replaced
`scripts/agent_installation.py`, `scripts/install-agents` and
`scripts/manage_installation.py` with their baseline versions, kept the new tests, and
ran them. Eight of the sixteen fail, which is exactly the set the diff touches:

```
FAILED test_agent_installer_preserves_an_unmanaged_definition
FAILED test_identical_unmanaged_definitions_are_adoptable_in_plan_check_and_install
FAILED test_agent_check_reports_foreign_markdown_entries
FAILED test_agent_actions_share_the_canonical_directory_link_exemption
FAILED test_agent_install_and_check_emit_managed_json_without_summary
FAILED test_agent_reconcile_and_uninstall_managed_remove_agent_links
FAILED test_agent_retirements_do_not_count_as_new_links
FAILED test_agent_install_accepts_an_empty_source_and_retires_all_definitions
8 failed, 8 passed
```

Weaknesses, none of them vacuity:

- `test_agent_install_and_check_emit_managed_json_without_summary`
  (`tests/test_install_agents.py:294-314`) asserts only the top-level key set. It would
  pass with every item mislabelled. Assert one state.
- `test_agent_check_reports_foreign_markdown_entries`
  (`tests/test_install_agents.py:268-279`) is the test that locks in MAJOR-1. It should be
  inverted to assert exit 0 with a warning.
- Nothing covers MAJOR-2 or MAJOR-3. Both are one-fixture tests.
- `test_agent_reconcile_and_uninstall_managed_remove_agent_links`
  (`tests/test_install_agents.py:316-333`) exercises `reconcile` only on an already-clean
  tree, where it is indistinguishable from `install`. `reconcile` has no behaviour of its
  own in this module; if that is intended, say so, because the skills surface gives it
  rename handling.

## Verified by execution

- `tests/test_install_agents.py` and `tests/test_install_harness.py` from the worktree
  root: **58 passed in 51.25s**.
- The eight-failure revert run above, from a scratch copy outside the worktree.
- Real-state simulation: copies of the three actual client agent files, hashes compared
  against the product sources (all three match), `install-agents` exits 0 and converts
  them to managed links with a receipt. The reported defect is genuinely fixed.
- Scenario A, personal extra `.md`: `install` rc 0, `check` rc 3, `plan` rc 0.
- Scenario B, three correct symlinks and no manifest: `plan`, `check` and `install` all
  rc 3 with a remove-the-correct-link remedy.
- Scenario C, dangling symlink at a managed name: rc 3, path untouched.
- Scenario D, directory where a file is expected: rc 3, directory untouched.
- Scenario E, managed entry replaced by a byte-identical file: `check` rc 0, `plan` rc 3,
  `install` rc 3.
- Scenario F, non-canonical directory symlink target: all four actions rc 3 with
  "target must not be a non-canonical symlink", matching the bash branch that
  `scripts/install-agents` deleted.
- Scenario G, `install-agents` end to end with a personal extra file: rc 3.

## Verified by reading only

- The `set -euo pipefail` propagation from `install-agents` rc 3 to `install-harness`
  failure; the shell semantics are unambiguous but I did not run the full harness install.
- The skills-surface comparison points in `scripts/manage_installation.py` (extras as
  warnings, `ok` key, `scope` fields, no content adoption).
- The unreachable `agent_installation = None` crash path in MINOR-3.
- The ADR and lifecycle prose accuracy.
- Crash-ordering of the manifest write relative to link publication in `install`.
