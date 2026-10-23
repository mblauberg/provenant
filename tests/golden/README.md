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
