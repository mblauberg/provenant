# Lane A integration handoff

The JSON files in this directory are additive `fabric.*.v1` examples. The validator
is `tests/test_fabric_v1_contract.py`; provider goldens and supervisor fixture
programs run in `tests/test_provider_exec.py` without model prompts.

## Reader contracts

- Canonical attempts: `tasks/<task_id>/attempt-NNN/attempt.json`. Existing artifacts
  and legacy receipts remain under `dispatch/tasks/<task_id>/attempt-NNN/`; use the
  canonical row's `paths`. Owner stdout retains the legacy record and adds `fabric`
  (the canonical row) and `digest`.
- `layout.run_root(cwd)` returns the primary checkout, or the resolved cwd outside
  Git. Runs live beneath `<root>/.agent-run/runs/`. `_owner/` is opt-in when creating
  the directory. Explicit legacy `mcp-*` directories remain accepted.
- Cooldowns use `{schema: "fabric.cooldowns.v1", cooldowns: {"adapter/model": record}}`.
  `FABRIC_COOLDOWNS_PATH` permits isolated fixtures. The run index is JSONL under
  `.agent-run/runs/index.jsonl`; terminal receipt closure follows publication.
- Routing calls the existing model-router CLI and consumes `snapshot --json` when
  available. `adapters` may be a map or a list. The owner performs fallback as new
  attempts in the same run. Legacy direct `cf_dispatch.sh --chain` remains a shell
  compatibility path; it does not gain owner attempt directories.
- Lane B must replace the assertion that every adapter has a Bash execution arm:
  execution is now in `provider_exec.py`, with declarative `adapters/*.py` profiles.
- Lane C can supply Kiro `read_only_probe` on a resolved route: `cli_version`,
  `checked_at` (ISO timestamp), `attempted_write: true`, `permission_denied: true`,
  `file_created: false`. The version must equal route `cli_version` and the probe
  must be less than 24 hours old. Without that evidence, Kiro reports `prompt_only`.
  Probe production and the capability cache belong to the routing lane.

## Conservative guarantees

Claude and Cursor writer guarantees are `best_effort`: approval flags alone do
not demonstrate filesystem confinement. Unsupported controls are warned about
and are not echoed as applied. A mismatched observed model has family `unknown`
until routing evidence attributes that model; it cannot certify cross-family
assurance. CLI version remains null when the route supplies no version evidence.

## Verification at lane completion

- Full Python suite: **2,012 passed, 1 failed** (374.66 seconds). The failure is
  `tests/test_harness_contract.py::test_readme_mermaid_parses_with_available_local_renderer`:
  Chromium fails its Mach bootstrap permission check in this sandbox.
- After the final watchdog and provenance regressions: focused supervisor,
  layout, contract, and metadata suite **87 passed** (9.65 seconds).
- Fabric TypeScript typecheck: **passed**.
- Fabric tests: **106 passed, 13 failed** (5 files; 47.84 seconds). One integration
  assertion is obsolete: `adapter-registry.test.ts` / `gives every implemented
  adapter an executing arm in the dispatcher`.
- The remaining 12 Fabric failures exercise process liveness; `/bin/ps` is denied
  with EPERM in this sandbox. Rerun outside the sandbox before merging:

  `execution-lifecycle.test.ts`:
  - `survives a cold start, so a fresh process can list the run`
  - `holds owner cleanup through host shutdown until a resistant provider stops`
  - `prints an unconfirmed stop reason from dispatch kill`
  - `fails dispatch kill after signalling only the owner of a live provider`
  - `signals the owner process group, so the provider child dies too`
  - `cancels a cold-start run before the attempt directory exists`
  - `leaves no running provider process after its MCP host is killed`
  - `keeps the owner record until an orphan that ignores SIGTERM is killed`
  - `does not mask a still-running orphan with the host-gone reason`
  - `never reaps a run whose host is still alive`

  `execution-pruning.test.ts`:
  - `never prunes a run still in flight`
  - `keeps an aged run while its recorded provider session is live`

Independent and cross-family reviews remain outstanding: both native agent
launches failed with `collab spawn failed: no thread with id`. Live provider smoke
tests were not part of this fixture gate.
