# Provider wire recordings

Decoder and activation evidence for the five retained providers in
[execution-route.md](../../../../../docs/specs/rust-port/design/execution-route.md).
[activation.json](activation.json) is the runtime gate: `providers::prepare`
refuses any execution mode it does not enable, and a missing mode is disabled.

## Coverage and activation

| Adapter | Recorded evidence | Read-only fresh/resume | Writer fresh/resume | Quota |
| --- | --- | --- | --- | --- |
| Claude | Live confined runs (2.1.310); success, model mismatch, matching model, continuation pair, authentication failure | Enabled | Enabled | Lane D |
| Codex | Live confined runs (0.159.0); success/usage, structured failure, interrupted prefix | Enabled | Enabled | Lane D |
| Cursor | Success, thinking deltas, continuation pair | Disabled | Disabled | Lane D |
| Agy | Init/step updates/success and error result | Disabled | Disabled | Lane D |
| OpenCode | Projected (version unknown): text/stop, error, tool-step finish then error and nonzero exit. Launch form prepared; live v2.0.32 stream lacked `step_finish` | Disabled | Disabled | Lane D |

## Live recordings

`<adapter>/live-<access>-<session>.jsonl` are complete streams from the
recorder (`crates/providers/examples/record.rs`, feature `recording`), which
runs the CLI under the compiled Rust confinement profile with a clean
environment and a trivial prompt, then probes every claimed permission.
`<adapter>/recordings.json` holds their manifest entries: CLI version,
platform, redacted argv, access and session mode, exit, decoded outcome and
answering model. `<adapter>/confinement.json` holds the probed observations per
access mode, which `prepare` attaches to the provider profile.

An enabled mode must cite a live capture with the same access and session mode,
marked `activation_eligible`, that decoded as success; a test enforces this, so
read-only evidence never enables a writer. Each live capture replays through
the real decoder, resumes bound to their fresh capture's continuation.
`claude/live-auth-required.jsonl` is not activation evidence: it records how a
missing or expired sign-in decodes (`Refused`).

Redaction substitutes session ids, paths under `HOME` or the recording root,
emails, `prefix_<id>` and UUID identifiers and token-shaped strings. Model
text, usage and timestamps are kept. Credentials are opened only by the CLIs;
probes read zero bytes from them.

## Projected recordings

The other files are sanitised projections of earlier tracked streams and
local receipts, listed in [manifest.json](manifest.json) with source, SHA-256,
substitutions and limitations. `null` there means unrecorded. They prove decoder
shapes only: no exact argv, platform or confinement form, so they enable
nothing. Their strings are replaced by stable `FIXTURE_<CLASS>_<NUMBER>`
placeholders and their numbers by zero. OpenCode's major version is unverified,
so its streams cannot establish v2 acceptance. OpenCode `step_finish` with
`reason: tool-calls` is an intermediate step, not terminal success.

Recovered retry, nested tool/subagent identity and quota windows remain gaps;
tests cover them with inputs labelled synthetic.
