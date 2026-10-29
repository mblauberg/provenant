# Release Notes

Use when writing or editing release notes, a changelog or release fragments (towncrier, changesets, a `next.d/` directory). Cross-project. The reader is an upgrader: should they upgrade, what must they do, what do they gain. Notes are clerical, not promotional.

## Rules

1. **Lead with the upgrade.** Order: intro, breaking changes, behaviour changes, additions, fixes. Breaking changes hold only entries that need a user action, and each states the action. A change that needs no action is a behaviour change.
2. **One to three sentences per entry.** Subject first (the command, flag or feature, in code font), one tense, one name per concept. State the user-visible change and its effect, not the mechanism. Failure sequences and tests belong in linked docs or the issue.
3. **Say standing policy once.** Stability, supported platforms and upgrade-together rules live on one stable page. Each release links to it in a line.
4. **Fixes name the symptom** and the new behaviour, not the root cause.
5. **Keep user-actionable numbers** (defaults, limits, units, flags). Drop internal ones (retry counts, schema versions) unless they change what the user must do.
6. **Leave out what no user notices:** refactors, tests, CI, contributor tooling, fixes to unreleased code, slice or phase numbers, process words.
7. **Say each thing once.** Merge entries on one topic, including repeats between the intro and an entry, and cite every issue on the merged entry.
8. **Reference, do not explain.** End with `(#123)` or `(#123, #124)`. Link a guide for anything longer.
9. **Tag, then group when large.** Each fragment carries an area tag line from a fixed project list. The collator renders area sub-headings only in a section with more than about a dozen entries, and lists smaller sections flat in area order. The area list belongs in the project's fragment guide.
10. **Deprecations and removals** name the replacement and the condition or version for removal.

## Template

```markdown
# Provenant v1.4.0

Released 4 March 2027.

What this release is about, in two sentences. Link to the stability policy.

## Breaking changes

- **`old-command` is removed.** Use `new-command`, which takes the same
  arguments (#123).

## Additions

- `export --format csv` writes the report as CSV (#125).

## Fixes

- `sync` no longer hangs when the remote closes the connection early (#126).
```

Omit empty sections.

## Rewrites

> Before: The scheduler no longer reads the wall clock. It uses the monotonic clock since boot, which no clock change can move, so two jobs never start less than one interval apart.
> After: Job spacing no longer changes when the system clock does.

## Changelogs and fragments

A changelog is the exhaustive record for developers and follows Keep a Changelog; release notes are curated for upgraders. Never paste a commit log. Fragment authors write the finished sentences a reader will see, one change per file, with no intro or policy. At release, one person reads the collated result end to end and writes the intro last.

## Checklist

- [ ] Every entry opens with its subject, runs one to three sentences and stands alone.
- [ ] Breaking changes need an action and state it; policy is linked, not pasted.
- [ ] No repeats, internal-only changes, process words, em-dashes or marketing words.
- [ ] When rewriting: every fact and issue number survives, and merges and drops are listed.

## Sources

Retrieved 29 October 2026: [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/); [Common Changelog](https://common-changelog.org/); [Google developer documentation style guide, What's new](https://developers.google.com/style/release-notes); [Django 5.2 notes](https://docs.djangoproject.com/en/5.2/releases/5.2/); [uv 0.9.0](https://github.com/astral-sh/uv/releases/tag/0.9.0); [Drew DeVault, How to write release notes](https://drewdevault.com/2021/05/19/How-to-write-release-notes.html).
