## Behaviour changes
### Lanes and settlement

- Lanes now run unsandboxed by default for every lane kind, including read-only lanes, so sub-agents can run natively like a CLI started by hand. The default `[domains.run] sandboxed_routes` list is now empty by default (users can configure routes to sandbox). Explicit `--no-sandbox` / knob `off` takes precedence over route, narrowed-network and Provenant parent floors, emitting a warning naming dropped restrictions. An operating-system inherited sandbox (Seatbelt) cannot be removed and is honestly reported as effective. Run records and receipts now track requested and effective sandbox mode and its source. See [lane sandbox mode](../../runbooks/configuration.md#lane-sandbox-mode).
