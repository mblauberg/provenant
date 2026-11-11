# Version-matched context

Version-sensitive guidance last checked 22 October 2026; verify against the installed package.

## Intent (installed-package skills)

- `intent list [--json]` finds skills inside `node_modules` of the current
  directory. `intent load <package>#<skill> [--path]` prints a skill, or its
  path. Both only read; neither created files in the project.
- Run from the app root with dependencies installed. Skills appear only for
  packages that ship them, and which ones do changes by release; check with
  `list`.
- A transitive package can list a different version from the one you adopted.
  Read the version column.
- Skill front matter `library_version` can lag the package version. Trust the
  package version, not that field.
- `npx -y` fetches Intent itself, so the first run needs the network. Its
  `list` and `load` do not call the registry (only `stale` does). Whether any
  other call is made was not tested offline.
- Writers, which change the app: `install` and `hooks install` write skill
  files and hooks, `setup` and `setup-github-actions` write workflow files,
  `edit-package-json` edits `package.json`, `exclude` edits ignore config, and
  `maintainer` is for package maintainers. Run one only when the user or the
  repository's instructions authorise that write.

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
- `create`, `add`, `add-on`, `template`, `starter`, `pin-versions` and
  `clean-demos` write project files, `dev` starts a server and `telemetry`
  changes settings. Run one only when the user or the repository's
  instructions authorise that effect.
- The CLI MCP server was not exercised; use the commands above.
