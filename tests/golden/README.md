# Golden CLI cases

Each file in `cases/<verb>/*.json` records one invocation of `provenant`.
The data is intentionally language neutral: it contains the argv array, stdin,
exit code, stdout, stderr, and changed files under the temporary fixture roots.
Text files store normalised content; binary files store a SHA-256 digest.

`run.py` uses only the Python standard library. It makes a temporary HOME,
XDG directories, Fabric roots, Codex and Claude directories, a fixture Git
repository with one run directory, and fake provider CLIs. The child receives
a small environment allowlist with credentials removed. `--record` rewrites
cases; pytest compares them and shows a unified JSON diff. `PROVENANT_GOLDEN_BIN`
selects the CLI; otherwise the Cargo release output or `provenant` on PATH is
used. Set `PROVENANT_GOLDEN_REQUIRED=1` to make a missing binary fail.

Cases cover help and usage, unknown verbs, root and a representative help plus
validation boundary for every command family listed by `provenant help`.
Aliases are recorded where they expose a separate spelling. Commands that can
launch real agents (`dispatch`, `batch`, `run`, and Fabric dispatch) are limited
to help, plan selection, or invalid/missing-state validation. The fixture's
fake provider CLIs are present for safe future invocation cases; no case
launches a provider or makes a network request. This records observable
behaviour at the CLI boundary, including the current delegates, for use as the
Rust port's contract.

Normalisation replaces temporary fixture and checkout paths, ISO timestamps,
durations, pids, UUIDs, run IDs, `.local` hostnames, and token-shaped strings.
