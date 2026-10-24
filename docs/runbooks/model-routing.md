# Model routing and cooldowns

Use `scripts/model-route routes --health` (or `--json`) to inspect available route pools,
quota and current cooldowns. `scripts/model-route cooldowns --json` returns only
the selected accounts on this host, using the same reader as MCP adapter status. Route configuration is instance-owned in
`config/model-routing.json`; [the model dossier](../model-dossier.md) describes
pool weights and model choices.

## Task classification

The remaining Python dispatch path uses explicit classes, configured rules
and a default. The utility-backed model classifier, shadow stages, routing-context
hook and output-digest hook have been retired. This old path remains until the
run/Fabric matrix gate; it is independent of Rust's explicit route resolver.

The Rust command is `provenant route list|resolve`. Resolve takes exactly one
`--model ADAPTER/ID` or `--route NAME`, optional `--effort` and at most two
explicit `--fallback` models. It performs no probe or state write. Named routes
live in the registered `[domains.routing.routes]` section of the fresh state's
`config.toml`. Availability never enables a form lacking provider activation
recordings. No model/effort is inferred and an unknown exact ID stays unverified.

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
