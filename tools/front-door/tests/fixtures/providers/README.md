# Provider wire recordings

All five adapters are enabled by default in every access and session mode with a launch form.
Recordings are replay and confinement probe evidence only; they do not gate execution.

## Live recordings

`<adapter>/live-<access>-<session>.jsonl` are complete streams from the
recorder (`crates/providers/examples/record.rs`, feature `recording`), which
runs the CLI under the compiled Rust confinement profile with a clean
environment and a trivial prompt, then probes every claimed permission.
`<adapter>/recordings.json` holds their manifest entries: CLI version,
platform, redacted argv, access and session mode, exit, decoded outcome and
answering model. `<adapter>/confinement.json` holds the probed observations per
access mode, which `prepare` attaches to the provider profile.

Each live capture replays through the real decoder, with resume captures bound
to their fresh capture's continuation. `claude/live-auth-required.jsonl`
records how a rejected sign-in decodes (`Refused`).

Redaction substitutes session ids, paths under `HOME` or the recording root,
emails, `prefix_<id>` and UUID identifiers and token-shaped strings. Model
text, usage and timestamps are kept. Credentials are opened only by the CLIs,
except Claude's access token, which the host reads outside the sandbox and
passes in the environment; probes read zero bytes from them.

## Projected recordings

The other files are sanitised projections of earlier tracked streams and
local receipts, listed in [manifest.json](manifest.json) with source, SHA-256,
substitutions and limitations. `null` there means unrecorded. They prove decoder
shapes only: no exact argv, platform or confinement form. Their strings are replaced by stable `FIXTURE_<CLASS>_<NUMBER>`
placeholders and their numbers by zero. OpenCode's major version is unverified,
so its streams cannot establish v2 acceptance. OpenCode `step_finish` with
`reason: tool-calls` is an intermediate step, not terminal success.

Recovered retry, nested tool/subagent identity and quota windows remain gaps;
tests cover them with inputs labelled synthetic.

`opencode/v2.2.0-success.jsonl` and `opencode/v2.2.0-free-limit.jsonl` project
the secret-free 2.2.0 captures supplied with #1975. Identifiers and timestamps
are replaced; the success stream has only `step_start` and `text`, and the
limit uses the nested `provider.free-limit` error. They establish decoder
shapes, not confinement or live provider availability. Dispatch tests pair
them with a synthetic `session_message` database with uncheckpointed WAL data.

`opencode/model-unavailable.jsonl` is a synthetic projection of upstream
2.2.0's `ModelUnavailableError`, mapped to `provider.no-route` by
[`toSessionError`](https://github.com/anomalyco/opencode/blob/v2.2.0/packages/core/src/session/to-session-error.ts#L88-L100)
and emitted as a JSON error by the noninteractive CLI. It contains no live
provider response.

`agy/command-denied.stderr` is the verbatim denial diagnostic reported in
issue #1776 from three read-only lanes on 2026-10-26. It records only stderr;
tests combine it with explicitly synthetic stdout frames or empty stdout.

`agy/denied-result.jsonl` is a synthetic stream frame using the denial shape
reported in [upstream issue #794](https://github.com/google-antigravity/antigravity-cli/issues/794).
