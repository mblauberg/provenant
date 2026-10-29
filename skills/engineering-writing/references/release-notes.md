# Release Notes

Use when writing or editing release notes, a changelog entry or a release fragment (towncrier, changesets or similar). Cross-project. For the changelog-versus-release-notes distinction see [document patterns](document-patterns.md#changelog-and-release-notes).

The reader is an upgrader. They want to know whether to upgrade, what they must do, and what they gain. Notes are clerical, not promotional: short, specific, in the user's terms.

## Principles

1. **Lead with the upgrade.** Breaking changes and required actions come first, then what is new, then fixes. A reader who stops after the first screen should still be safe.
2. **Say what changed for the user, not how it works.** Name the command, flag, key or behaviour and its effect. Leave out the mechanism, the failure sequence and the test that proved it. Put depth in the linked issue, ADR or reference page.
3. **Say it once.** A standing policy (stability, supported platforms, versioning, upgrade-together rules) lives in one stable page. Each release links to it in a line, or mentions it in the upgrade note when the release breaks something. Never paste a boilerplate section into every release.
4. **Every entry stands alone.** It makes sense without its heading, names its subject, and is not a continuation of the entry above.
5. **Every breaking change carries its action.** Say what stopped working, who is affected and what to do instead. If nothing is needed, say so.
6. **Leave out what no user notices.** Internal refactors, test changes, CI, dependency bumps with no effect, and fixes to unreleased code are not release notes. Combine related changes into one entry.
7. **One voice for the whole document.** Fragments or commits are raw material. The published notes read as one document with one tense, one set of terms and one level of detail.

## Template

```markdown
# 1.4.0

One or two sentences: what this release is about. A one-line link to the
stability policy. Upgrade note, only when something breaks.

## Breaking changes

- **`old-command` is removed.** Use `new-command`, which takes the same
  arguments. Scripts that call `old-command` fail with exit status 2 (#123).

## Behaviour changes

- `list` sorts by last use instead of name (#124).

## Additions

- `export --format csv` writes the report as CSV (#125).

## Fixes

- `sync` no longer hangs when the remote closes the connection early (#126).
```

Omit an empty section. Large releases may group entries under area sub-headings (as Django and Go do) once a section passes about a dozen entries. A release with no breaking changes says so in the intro only if readers would otherwise wonder.

## Entry rules

- **Tense and verb.** Describe the new state in the present tense (`list now sorts by last use`) or lead with a past-tense or imperative verb (`Added`, `Add`), and keep one choice for the whole document. Common Changelog uses imperative verbs; Google's style guide uses past tense for changes. Never mix.
- **Length.** One to three sentences. A breaking change may take four to five to cover action and scope. If an entry needs a paragraph, it is two entries or it belongs in a guide.
- **Subject first.** Open with the command, flag, key or feature, in code font. `` `lanes --wait` now exits when ... `` beats `Fixed an issue where ...`.
- **Fixes name the symptom.** `` `status` no longer reports a finished run as running `` states the old failure and the new behaviour. Do not narrate the root cause.
- **Numbers and limits that users can act on stay** (defaults, caps, units, versions, flags). Internal numbers (retry counts, timeouts users cannot set, schema versions) go unless they change what users must do.
- **No hedging stacks.** Put one caveat in one sentence. Do not list every case that does not change.
- **References.** End the entry with `(#123)`. When one entry covers several issues, list them: `(#123, #124)`. Link to a guide for anything longer than the entry.
- **No process words.** No slice numbers, wave names, `this PR`, `we now`, review-gate language or agent names.
- **Terms.** One name per thing, matching the CLI, config and docs. Do not rename a concept between entries.
- **Order.** Within a section, breaking first, then by user impact. Group related entries together.
- **Deprecations and removals** name the replacement and the version or condition for removal.

## What to leave out

- Internal refactors, renamed internals, test and CI changes, and fixes to code that never shipped.
- The failure sequence, race window or data structure behind a fix. One clause of symptom is enough.
- Per-entry reassurance that something else is unchanged. Say it once, in the intro, if it matters.
- Slice, wave or phase numbers and ticket-tracker vocabulary. Issue numbers go in parentheses, as references.
- The same entry in two sections. Pick the one a reader would look in.

## Agent-written notes

Agents tend to write release notes the way they write commit bodies: exhaustive, mechanism-first and cautious. Counter this directly.

- State the user-visible effect first. If an entry still reads like a design note, cut it to its first sentence and check that it carries the fact.
- Do not append a clause for every edge case the change handles. Edge cases belong in the reference docs or tests.
- Do not paste the previous release's boilerplate into a new one. Link the standing policy.
- When rewriting existing notes, preserve every fact a user can act on, every issue reference and every breaking change. Change presentation, not substance, and list any entry you merged or dropped so a reviewer can audit it.

## Fragment workflows

A fragment tool (towncrier, changesets, a `next.d/` directory) gives each change its own file, so the collated result can read as a pile of voices. Fragment authors write for the final document:

1. Write the finished sentence a reader will see, not a note to the maintainer.
2. Follow the entry rules above. One fragment, one user-visible change, one to three bullets.
3. Do not restate policy, add version intros or write headings beyond the allowed set.
4. A change that matters to nobody outside the code needs no fragment (towncrier's `misc` type is for this).
5. At release, the collator or editor reads the whole result once, merges entries on the same topic, aligns tense and terms, and writes the intro last.

## Before and after

Internal detail removed (mechanism and test narrative to one user-visible fact):

> Before: Startup spacing no longer depends on the wall clock. It is measured on the host's monotonic clock since boot, which every process on the host shares and no clock change can move, and each class's last start is kept with the boot it was read in. Within one boot, no two lanes of a class start less than one spacing apart, whatever the wall clock does, including a step back then forward or a step forward alone. [...]
>
> After: Lane startup spacing no longer changes when the system clock does. A reboot resets spacing, so the first lane of each class after boot starts at once.

A repeated policy block replaced by a link:

> Before: `## Stability` with the same five-sentence "There is no stability promise..." paragraph in every release.
>
> After: `Provenant makes no stability promise; see [stability](README.md#stability).` One line in the intro, and the full text once in the notes index.

A fix with the failure story trimmed to symptom and effect:

> Before: `provenant install` now restarts the peer LaunchAgent after an upgrade. launchd keeps the code requirement of the binary it first loaded, so after the release binary changed, `kickstart` left the agent unable to spawn (exit 78). When kickstart does not produce a new running process, install now boots the agent out and bootstraps it again (#1926).
>
> After: `provenant install` restarts the peer LaunchAgent after an upgrade. Previously the agent could fail to start with exit status 78 (#1926).

## Checklist before publishing

- [ ] The intro says what the release is about in two sentences or fewer.
- [ ] Breaking changes come first, each with its action. Nothing breaking sits in another section.
- [ ] No section repeats text from another release. Standing policy is linked, not pasted.
- [ ] Every entry opens with its subject and stands alone.
- [ ] One tense and one set of terms throughout. No em-dashes, no marketing words, no process words.
- [ ] No entry exceeds three sentences except a breaking change.
- [ ] No internal-only change is listed. Related changes are merged.
- [ ] Every issue number from the source fragments survives, and no breaking change was dropped.
- [ ] The whole document read end to end as one piece, intro written last.
- [ ] Links resolve and spelling passes the repository's checks.

## Sources

Retrieved 29 October 2026.

- **Keep a Changelog 1.1.0** ([keepachangelog.com](https://keepachangelog.com/en/1.1.0/)): for humans; no raw commit logs; list deprecations, removals and breaking changes.
- **Common Changelog** ([common-changelog.org](https://common-changelog.org/)): one-line imperative entries that stand alone; `**Breaking:**` first; one notice line per release; omit noise; merge related changes.
- **Google developer documentation style guide, What's new** ([developers.google.com/style/release-notes](https://developers.google.com/style/release-notes)): past-tense verbs for changes, describe the change not its location, omit minor corrections, give reasons for removals.
- **Microsoft Writing Style Guide, writing tips** ([learn.microsoft.com](https://learn.microsoft.com/en-us/style-guide/global-communications/writing-tips)): short sentences, active voice, one word per concept.
- **Go 1.24 release notes** ([go.dev/doc/go1.24](https://go.dev/doc/go1.24)): the compatibility promise stated once in the introduction; entries that change behaviour say how to restore the old one.
- **Rust 1.87.0 announcement** ([blog.rust-lang.org](https://blog.rust-lang.org/2025/05/15/Rust-1.87.0/)): short feature entries linking out for depth; the one breaking change states rationale and action.
- **Django 5.2 release notes** ([docs.djangoproject.com](https://docs.djangoproject.com/en/5.2/releases/5.2/)): upgrade pointer first; "Backwards incompatible changes" section with fixes; one-to-two-sentence minor entries.
- **uv 0.9.0 release** ([github.com/astral-sh/uv](https://github.com/astral-sh/uv/releases/tag/0.9.0)) and **uv versioning policy** ([docs.astral.sh](https://docs.astral.sh/uv/reference/policies/versioning/)): breaking changes first with what changed, who is affected and how to pin the old behaviour; other entries one line with a PR link; policy on a stable page.
- **GitHub changelog** ([github.blog/changelog](https://github.blog/changelog/)): short declarative headlines with a type label (release, improvement, retired).
- **Towncrier tutorial** ([towncrier.readthedocs.io](https://towncrier.readthedocs.io/en/stable/tutorial.html)): one fragment per change by type; `misc` fragments stay out of the notes.
- **Changesets** ([github.com/changesets/changesets](https://github.com/changesets/changesets/blob/main/docs/adding-a-changeset.md)): a summary says what changed, why and how consumers update.
- **Drew DeVault, How to write release notes** ([drewdevault.com](https://drewdevault.com/2021/05/19/How-to-write-release-notes.html), 19 May 2021): readers want upgrade actions first, then highlights; notes facilitate upgrades rather than advertise.
