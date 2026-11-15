## Behaviour changes

### Telemetry and providers

- Native cache binding keeps default npm, pnpm and Corepack package caches on host locations when provider `HOME` or `XDG_*` paths would split them. The selected pnpm version chooses `npm_config_*` selectors before version 11 and `pnpm_config_*` selectors from version 11; unresolved shims preserve configured selectors and warn instead of guessing. npm can report the legacy selectors as unknown settings in the same provider session. Set `cacheDir` and `storeDir` in `pnpm-workspace.yaml` or supported global `config.yaml` to avoid those environment selectors. (#3326)
