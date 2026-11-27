# Continuity contract

## Standing record

Open the checkpoint with the typed standing record in its frontmatter. Every
top-level field is required, so an omitted `waiting_on_owner` never reads as
"nothing waits"; empty lists are explicit:

```yaml
---
provenant_standing:
  schema_version: 1
  goal: "Land issue 1234 with CI green"
  grants:
    - scope: "Land lane PRs into main once CI passes"
      source: "owner:https://github.com/org/repo/issues/1234#issuecomment-1"
      expires: none
  obligations: ["Report each settled lane"]
  waiting_on_owner: []
---
```

A grant's `source` is one token: `owner:<ref>` for where the owner gave it (an
issue comment, a transcript turn) or `rule:<ref>` for a durable rule (a
repository path and anchor). Prose, or no source, leaves the grant unverified.
`expires` is an RFC 3339 instant or `none`; an expired grant is not restored.
Keep the scope as the owner bounded it. `waiting_on_owner` names each specific
ask. A save replaces the whole file, so carry the record forward, dropping
grants and asks that ended. Hooks restore the record ahead of the prose at
session start, after compaction and after an account switch.

The chair's standing grants — maintainer ownership, release, admin-merge, no
local builds and the CI cap — live in this typed record, each with its source
and expiry. Memory prose is not their owner: it may explain why a grant exists,
but a hook restores only what this record carries. `session save
--from-template` scaffolds the record and carries it forward from the previous
revision; a save that omits it warns and names the gap instead of refusing.

## Body

The [checkpoint template](../templates/checkpoint.template.md) lists the
headings. Link live lanes and criteria to their owners instead of copying
changing status.

The Intent section is the chair's understanding, not quotation. For each live
ruling record what the user wants, the reason as you understand it, rejected
alternatives, open disagreement and any reading you are unsure of. A
correction replaces the earlier reading. Quote the user only where wording
matters: an authority grant, or a correction of a misreading. Hooks show this
section first after compaction and name the host transcript for checking an
entry in doubt; save it at each ruling or correction and before a long task.

Retain rulings, authority, evidence pointers, open questions and the next
action. Drop transcripts, tool output and anything that can be reopened. The
PreCompact summary instruction separately drops summary items untouched for
24 h unless they are owner asks and moves them to the tracker; that is guidance
for the summary, not a change to what the saved checkpoint retains.
