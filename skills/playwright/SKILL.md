---
name: playwright
description: "Use for terminal-driven browser navigation, interaction, snapshots, screenshots, extraction, or UI-flow debugging. Not for authoring Playwright test suites or frontend design; use tdd or ui-ux-design."
---


# Playwright CLI Skill

Drive a real browser from the terminal with `playwright-cli` and the bundled
wrapper. Do not pivot to `@playwright/test` unless explicitly asked for tests.

## Prerequisite check (required)

Prefer an existing `playwright-cli`. The wrapper fails closed rather than
installing from the network unless the user authorises package retrieval:

```bash
command -v playwright-cli || command -v npx
```

If both are absent, report the missing capability. Do not install Node or a CLI
without authority. For an authorised one-run npx resolution, set:

```bash
PLAYWRIGHT_CLI_ALLOW_NPX_INSTALL=1 "$PWCLI" --help
```

An existing PATH binary is used as installed; the wrapper does not attest its
version. When lineage matters, record `command -v playwright-cli` and
`playwright-cli --version`. Only the authorised `npx` fallback is package-pinned;
`--help` proves available commands, not version identity.

## Skill path (set once)

```bash
export PWCLI="<installed-playwright-skill>/scripts/playwright_cli.sh"
```

Replace `<installed-playwright-skill>` with the absolute skill directory
disclosed by the client that loaded this skill.

## Core workflow

1. Open the page.
2. Snapshot to get stable element refs.
3. Interact using refs from the latest snapshot.
4. Re-snapshot after navigation or significant DOM changes.
5. Capture only assigned artifacts; close the run-owned session.

Minimal loop:

```bash
"$PWCLI" open https://example.com
"$PWCLI" snapshot
"$PWCLI" click e3
"$PWCLI" snapshot
```

## References

Open only what you need:

- CLI command reference: `references/cli.md`
- Practical workflows and troubleshooting: `references/workflows.md`

## Guardrails

- Before submit, send, purchase, delete, account/config mutation, upload or any
  other external effect, resolve action-specific authority and target. Opening
  or inspecting a page does not authorise the effect.
- Never place real secrets in command arguments. Inventory whether the session
  is signed in; prefer synthetic accounts/data and an isolated named session.
- Screenshots, snapshots, downloads and traces may contain credentials, personal
  data, DOM state, console/network content or tokens. Write only to an assigned
  run path, declare retention, and remove only proven run-owned artifacts.
- Always snapshot before referencing element ids like `e12`.
- Re-snapshot when refs seem stale.
- Prefer explicit commands over `eval` and `run-code` unless needed.
- When you do not have a fresh snapshot, use placeholder refs like `eX` and say why; do not bypass refs with `run-code`.
- Use `--headed` when a visual check will help.
- Close run-owned sessions and report any session/artifact retained for handoff.
- Default to CLI commands and workflows, not Playwright test specs.
