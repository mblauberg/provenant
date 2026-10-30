# Release notes

Released versions, newest first:

- [1.4.0](v1.4.0.md)
- [1.3.0](v1.3.0.md)
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

- **First line.** Use `## Breaking changes`, `## Behaviour changes`,
  `## Additions` or `## Fixes`. The collator normalises case and whitespace;
  an unknown heading is warned about and treated as Additions. Later `##`
  headings are warned about and ignored.
- **Second line (area).** `### <Area>`, from the project list below. Tag every
  fragment; an unknown area is warned about and treated as untagged. The
  collator decides whether to show the tag: it renders `### Area` sub-headings,
  in the order below, only in a section with more than 12 entries, and lists a
  smaller section flat in the same order, with untagged entries last.
  1. CLI and install
  2. Lanes and settlement
  3. Git and safety
  4. Accounts, quota and routing
  5. Pipelines and graphs
  6. Chair, inbox and console
  7. Fabric and MCP
  8. Telemetry and providers
- **Body.** One bullet per change, each one to three sentences for the
  published notes: subject first, in the user's terms, ending with `(#issue)`.
  A breaking change states the action: `**The change.** What to do.`
- **Upgrade actions.** Anything a user must do on upgrade is a breaking change.
  A change that needs no action never goes under Breaking changes.
- **Do not include.** Stability or version boilerplate, an intro, mechanism or
  failure-sequence detail, internal-only changes, or entries copied from commit
  messages. A change no user can notice needs no fragment.
- **Links.** Paths are relative to `next.d/`. Link to a guide or ADR for depth
  instead of explaining it in the entry.
- **Intro.** `next.d/_intro.md` is two sentences on what the release is about,
  a one-line link to the Stability section above, and a pointer to Breaking
  changes if the release has any. Maintainers write it last.
