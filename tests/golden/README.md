# Golden CLI cases

Each `cases/<verb>/*.json` captures argv, stdin, a case `class`, exit code,
stdout, stderr and changed files in a fresh fixture. The stdlib-only runner
uses a temporary HOME, XDG and Fabric directories, a Git repository with a
fixed initial commit, fake providers and an offline `gh`. PATH contains only
fixture tools: pinned Git, the harness's Python, Bash and dirname. Delegates
also receive that Python through `HARNESS_PYTHON`; bytecode writes are disabled.
Fabric queries use the installed Node executable pinned through `FABRIC_NODE`.
Credentials and host configuration are excluded. The product checkout's Git
status must remain unchanged, excluding `.agent-run`.

`help` cases cover help, usage and argument errors. They compare exit code and
the sets of subcommands and flags parsed from combined output; stdout/stderr
are retained as reference text. `machine` cases compare exit code, normalised
stdout/stderr and changed files byte for byte. Text files retain normalised
content and binary files retain SHA-256 digests. Git's internal index and object
metadata are excluded; worktree `.git` pointer files are included. SQLite
snapshots contain sorted `sqlite_master` schema SQL and rows ordered by every
column, rather than `iterdump()` output.

`manifest.json` pins Python major.minor and SQLite version. CI selects Python
3.12 through uv and invokes the suite with its `.venv/bin/python`; a runtime
mismatch fails before case comparison with a re-record instruction.
`PROVENANT_GOLDEN_BIN` selects the binary, otherwise only the Cargo release
output is accepted (`CARGO_TARGET_DIR` is honoured). A missing binary fails;
`PROVENANT_GOLDEN_ALLOW_SKIP=1` explicitly permits skipping, unless
`PROVENANT_GOLDEN_REQUIRED=1` is set.

Every public command listed by `provenant help` must have a case, and the
recorded file set must exactly match the generated set. Real success paths
cover worktree create/list/remove, catalog route resolution and route listing,
branch lint, event reading and Fabric dispatch listing. Worktree list/remove
cases have fixture-local setup commands, and events reads a seeded terminal
attempt receipt in `run.py`. Gate's only
execution mode runs the heavy composite suite, so it has help coverage only.
Fabric dispatch has no public plan/dry-run mode: `fabric-dispatch-invalid-option`
covers an invalid flag and `fabric-dispatch-list` is a real read-only query.
Provider-launching verbs stay at help or validation boundaries.

Normalisation replaces fixture/checkout paths (including macOS `/var` aliases),
timestamps, durations, explicit numeric PID fields, UUIDs and whole hostname
tokens. Run IDs and tokens retain their character-class shape, separators and
length so format changes remain visible. Normaliser unit tests cover token
boundaries and formats.

## Updating

Re-record all cases using CI's interpreter (append `--verb route` for one verb):

```sh
uv run --python 3.12 --group test python tests/golden/run.py --record
```

Recording prunes orphan case files. Partial updates require the existing
manifest's runtime; use a full record when changing the runtime. Review
`git diff -- tests/golden` for changed exit codes, machine output, file effects,
help command/flag sets and manifest versions. Then compare:

```sh
uv run --python 3.12 --group test python -m pytest tests/golden/test_golden_cli.py -q
```

## Differential runs

Compare two implementations without rewriting the goldens:

```sh
uv run --python 3.12 --group test python tests/golden/diff.py --a /absolute/python-front-door --b /absolute/rust-front-door
```

`PROVENANT_A` and `PROVENANT_B` can supply the binaries instead. To compare
implementation lanes in one binary (defaults to the J1 binary selection):

```sh
uv run --python 3.12 --group test python tests/golden/diff.py --a /absolute/provenant --impl-env PROVENANT_LANE_IMPL=python,rust
```

Each side gets its own fresh fixture and the same normalisation and per-class
comparison as J1. Use `--verb route` or `--case root-case-01` to filter; each
option can be repeated, and combining them takes their intersection.
`--jobs N` runs cases concurrently (default 1).

Reports go to `.agent-run/diff/<UTC timestamp>/`, or the new directory supplied
with `--output-dir`. `divergences.jsonl` contains one row per differing field
with case, verb, field, bounded A/B values and a short unified diff.
`summary.json` counts cases and allowed/unexpected divergences per verb.
Machine fields are `exit_code`, `stdout`, `stderr`, `files.paths` and
`files.<path>`; help cases compare `exit_code` and the parsed `help` sets.
Setup/execution failures appear as `execution_error` and always fail, including
when both sides fail identically or the field is allowlisted. Implementation
selectors cannot override fixture-owned environment variables.

`diff-allow.json` is an array of intended divergences. Each entry needs an
exact case name, field and reason; optional `a` and `b` regular expressions
constrain the full, untruncated values (regex search semantics):

```json
[
  {
    "case": "root-case-01",
    "field": "stdout",
    "reason": "Example of an intentional root representation change",
    "a": "^<CHECKOUT>\\n$",
    "b": "^checkout:<CHECKOUT>\\n$"
  }
]
```

Use `--allowlist PATH` for an alternative file. Allowed divergences are logged
with their reasons. Any unexpected divergence exits 1; an allowlist entry that
never matches is a stale-entry warning and does not fail the run. A filter can
therefore warn about entries outside the selected cases.

The fast smoke test covers identical binaries, one changed stdout field, an
allowlisted change, constrained matching, stale entries, parallel cases and
implementation-env mode:

```sh
uv run --python 3.12 --group test python -m pytest tests/golden/test_diff.py -q
```

This smoke test skips when no J1 binary is available unless
`PROVENANT_GOLDEN_REQUIRED=1` is set.
