# Version-matched context

Verified against `@tanstack/intent` 0.5.6 and `@tanstack/cli` 0.72.0 in a
scratch project holding the adopted package versions.

## Intent (installed-package skills)

- `intent list [--json]` finds skills inside `node_modules` of the current
  directory. `intent load <package>#<skill> [--path]` prints a skill, or its
  path. Both only read; neither created files in the project.
- Run from the app root with dependencies installed. Skills appear only for
  packages that ship them. In the scratch project Router, router-plugin,
  Table, Hotkeys, Devtools and DB shipped skills; Query, Form, Store, Pacer and
  Virtual did not. Re-check with `list`, as this changes by release.
- A transitive package can list a different version from the one you adopted
  (the scratch run showed Devtools 0.15.0 under react-devtools 0.10.13, and
  Hotkeys core 0.18.0 under react-hotkeys 0.17.0). Read the version column.
- Skill front matter `library_version` can lag the package version. Trust the
  package version, not that field.
- `npx -y` fetches Intent itself, so the first run needs the network. Its
  `list` and `load` do not call the registry (only `stale` does). Whether any
  other call is made was not tested offline.
- Writers, never run in an app: `install`, `hooks install`, `setup`,
  `setup-github-actions`, `edit-package-json`, `exclude`, `maintainer`.

## CLI (docs only)

- Prefix every call with `DO_NOT_TRACK=1 TANSTACK_CLI_TELEMETRY_DISABLED=1`.
  The CLI reads both variables.
- Read-only, network needed: `libraries [--json]`, `doc <id> <path>
  [--docs-version v5]`, `search-docs "<q>" --library <id> --framework react
  --limit <n>`, `ecosystem`.
- `search-docs` without `--framework` returns Vue, Solid and Preact pages.
- Docs resolve to `latest` or a major (`v5`), never the installed patch. A page
  can describe an API newer than the installed version, so check the installed
  type declarations (`dist/*.d.ts`) before using a symbol.
- Do not run `create`, `dev`, `add`, `add-on`, `template`, `starter`,
  `pin-versions`, `clean-demos` or `telemetry`.
- The CLI MCP server was not exercised; use the commands above.
