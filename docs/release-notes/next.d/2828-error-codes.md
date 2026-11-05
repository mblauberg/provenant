## Breaking changes
### Fabric and MCP

- **Error codes use uppercase spellings.** Update clients matching `launch_unknown`, `owner_local_only`, `unsupported_owner`, `console_app_unavailable`, `reconcile_incomplete` or `cli_failed` to match `LAUNCH_UNKNOWN`, `OWNER_LOCAL_ONLY`, `UNSUPPORTED_OWNER`, `CONSOLE_APP_UNAVAILABLE`, `RECONCILE_INCOMPLETE` or `CLI_FAILED` respectively (#2828).
