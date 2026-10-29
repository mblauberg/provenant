## Fixes
### Lanes and settlement

- Lanes no longer override the pnpm store or `PNPM_HOME`. pnpm records the store directory in `node_modules/.modules.yaml`, so the override made a worktree the chair had installed natively look stale and the lane tried to reinstall into a store it cannot write. Lanes leave both unset, read the default host store without writing it, and skip pnpm's pre-run dependency check, so `pnpm run <script>` uses the native install. Corepack also ignores the `packageManager` field and refuses downloads, so a pinned pnpm cannot arrive as a non-executable placeholder (#2040, #1944).
