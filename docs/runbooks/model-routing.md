# Model routing and cooldowns

Use `scripts/model-route routes --health` (or `--json`) to inspect available route pools,
quota and current cooldowns. `scripts/model-route cooldowns --json` returns only
the selected accounts on this host, using the same reader as MCP adapter status. Route configuration is instance-owned in
`config/model-routing.json`; [the model dossier](../model-dossier.md) describes
pool weights and model choices.

## Task classification

`task_classification` in the routing config controls the advisory classifier
chain (`explicit`, `rules`, `model`, `default`), its shadow stages and fallback.
Rules match mode, role, prompt terms, paths and size. The model stage uses the
utility classifier with the task summary and configured class names and
descriptions; it caches results by prompt hash under the agent state directory.
Errors, timeouts and low confidence fall through. A call can set
`--classify rules,default`, or use `PROVENANT_TASK_CLASSIFY`, to replace the
chain. Class names, descriptions and class-to-tier choices live in
`task_class_routes`.

Class and size determine tier; role is a label. Missing role uses the worker
label with a warning. The former Python hook configurator is retired; the
installer manages recorded hook registrations through the CLI's `HookSpec`
policy and fixtures.

Retired: `provenant telemetry ingest` and its ledger workflow below;
[run integration step 2](../specs/rust-port/design/telemetry.md#tests-and-build-sequence) owns caller removal.

To compare classifiers, leave rules and model in `shadow`, then inspect the
`classified_by`, `classifier_confidence` and `shadow_answers` columns in the
local `usage` ledger after `provenant telemetry ingest`. Compare agreement and
outcomes by class before changing the active chain. The report view is not yet
updated to summarise these fields.

## Account and host scope

Quota and cooldowns share one resolver in `scripts/provider_identity.py`:

| Adapter | Identity directory or source |
|---|---|
| claude | `CLAUDE_CONFIG_DIR`, else `~/.claude` |
| codex | `CODEX_HOME`, else `~/.codex` |
| agy | On macOS the OS user's login Keychain, ignoring HOME and `AGY_CONFIG_DIR`; elsewhere `~/.gemini/antigravity-cli` |
| cursor | `~/Library/Application Support/Cursor/User/globalStorage` on macOS, else `~/.config/Cursor/User/globalStorage` |
| opencode | `$XDG_DATA_HOME/opencode`, else `~/.local/share/opencode` |
| kiro | `~/.kiro` |
| copilot | `~/.copilot` |

Configure non-secret labels in `accounts.<provider>.<label>.identity_dir` in
`<instance-root>/config/telemetry.json`, as shown in [telemetry](telemetry.md).
An enabled matching label wins; otherwise the default directory is `default`
and another resolved directory gets a stable `path-<sha256 prefix>` key.
Unresolved identity is null and matches only null. Host is `local_host` in
`.agent-fabric/hosts.json`, falling back to the hostname's slug. Attempts capture account
and host before provider startup, and retain them through completion.

Switching the selected identity or host hides the previous scope's cooldowns;
switching back restores them until expiry. Reads migrate legacy null-account
rows without a host to the default identity on the current host, once, under
the existing store lock. Exact model cooldowns precede `*`. Agy and OpenCode
quota failures remain per model. Quota readings affect only the same account
and host. See the [Fabric spec](../specs/fabric-v2.md) for the storage contract.

Agy sign-in is shared through the login Keychain on one Mac. Another HOME or
`identity_dir` does not create an isolated account. Use the workshop Mac for
the second agy account and distinct host ids for the two Macs.

## Reset a cooldown

1. Inspect `scripts/model-route routes --health` and confirm the intended account label.
2. Reset one model or adapter on this host:

   ```sh
   scripts/model-route reset claude/sonnet --account max-a
   scripts/model-route reset codex --account work
   scripts/model-route reset --account work
   ```

   Aliases are canonicalised. `--account` keeps other accounts and hosts intact.
   Without it, existing behaviour remains: `reset adapter/model` clears that
   model across scopes, `reset adapter` clears the adapter, and bare `reset`
   clears all cooldowns. Add `--json` to print the removed route keys.
3. Inspect the route again. A still-exhausted cached quota reading can reapply
   its account's cooldown until the provider-reported reset.
