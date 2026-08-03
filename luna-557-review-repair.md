# Luna 557 repair report

Baseline: `4d28ea7f` exact HEAD. No commit, GitHub, trust, install, daemon, or global-state actions were performed.

## Repairs

1. `run_dir_finalize.py` and `delivery_receipt_lifecycle.py` now derive primary certification eligibility from the canonical certifying `provider_assurance` values only: `full-vendor-identity` and `lockfile-install-attestation`. Missing, unknown, advisory, and owner-controlled assurance fails closed. The receipt tests cover advisory assurance paired with a malicious `certification_eligible: true` boolean.

2. Added `provider-assurance-result-shape.v1` to the existing negotiated result-shape feature registry and agent initialize offers. Provider assurance is optional on legacy wire shapes and enforced by the existing result-shape gate when the feature is negotiated. Route, terminal, evidence, currency, and slot codecs and generated schemas follow that compatibility boundary. Fixtures cover absent-token legacy results, rejected missing fields under the token, matching-token results, and rejected unnegotiated fields.

3. `cf_dispatch.sh` now retries the owner adapter-executable lookup without `--json` only for explicit unsupported-option diagnostics. A successful legacy plain-path response remains verified-owner but has unknown assurance and cannot certify. Non-option hard failures retain the existing rejection path.

## Verification

- Protocol package typecheck: PASS.
- Protocol build and generated schema write: PASS.
- Generated schema check: PASS.
- Protocol tests: PASS, 55 files and 909 tests; new result-shape fixture: 2 tests passed.
- Receipt suites: PASS, 93 tests.
- Dispatcher repair-focused tests: PASS, 2 tests.
- Bash syntax, Python compilation, and `git diff --check`: PASS.

The combined focused Python invocation recorded 131 passes and one unrelated environment failure: the non-Git model-route fallback test forces its subprocess to the active Python 3.14 interpreter, which lacks `yaml` (`ModuleNotFoundError: No module named 'yaml'`). No dependency was installed.

The repository-root `npm run build` remains blocked outside this repair at three existing imports of `MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE` in `runtime/agent-fabric`; TypeScript reports that the `@local/agent-fabric-protocol` package has no exported member. The protocol workspace itself typechecks and builds successfully. No unrelated source was changed to mask this blocker.

The requested report path under `/Users/user/.agents/.agent-run/` was outside this worktree's writable sandbox, so this report is stored at the worktree root for chair inspection.
