# Release notes

Released versions, newest first:

- [1.2.1](v1.2.1.md)
- [1.2.0](v1.2.0.md)
- [1.1.0](v1.1.0.md)
- [1.0.0](v1.0.0.md)

[Pending fragments](next.d/) describe changes for the next release. The
[maintenance procedure](../../MAINTAINING.md#change-history) covers collation
with [the release-note script](../../scripts/collate-release-notes) and updating
this version list when publishing a release.

## Stability

Provenant makes no stability promise. Commands, flags, `--json` output,
`config.toml` keys, the state layout and Fabric MCP tool names and schemas can
change in any release, with no deprecation period and no compatibility shims.
Upgrade every host together, because peers on different versions are not
supported. Each release lists its breaking changes first. This is the only
place the policy is stated; release notes link here and do not repeat it.

## Writing a fragment

Follow the [release-notes reference](../../skills/engineering-writing/references/release-notes.md).
Add one file per user-visible change under `next.d/`, named `<issue>-<topic>.md`.

```markdown
## Fixes
### Lanes and settlement

- `status` no longer reports a finished run as running (#123).
```

- **First line.** Exactly one of `## Breaking changes`, `## Behaviour changes`,
  `## Additions` or `## Fixes`. No other `##` heading is allowed.
- **Second line (area).** `### <Area>`, from the list below. The collator groups
  each section's entries by area in this order and leaves out empty areas.
  Untagged fragments come last, under no sub-heading, so tag every fragment.
  An unknown area fails `scripts/collate-release-notes --check`.
  1. CLI and install
  2. Lanes and settlement
  3. Git and safety
  4. Accounts, quota and routing
  5. Pipelines and graphs
  6. Chair, inbox and console
  7. Fabric and MCP
  8. Telemetry and providers
- **Body.** One to three bullets, each a finished sentence for the published
  notes: subject first, in the user's terms, ending with `(#issue)`. A breaking
  change also says who is affected and what to do instead, in up to five
  sentences: `**The change.** Action.`
- **Upgrade actions.** Anything a user must do on upgrade (drain lanes, upgrade
  every host together, edit config, rerun a command) is a breaking change, or
  goes in the intro's upgrade note. Never leave one under Behaviour changes.
- **Do not include.** Stability or version boilerplate, an intro, mechanism or
  failure-sequence detail, slice or wave numbers, internal-only changes, or
  entries copied from commit messages. A change no user can notice needs no
  fragment.
- **Links.** Paths are relative to `next.d/`. Link to a guide or ADR for depth
  instead of explaining it in the entry.
- **Intro.** `next.d/_intro.md` is two sentences on what the release is about,
  a one-line link to this page's Stability section, and an upgrade note only if
  the release breaks something. Maintainers write it last.
