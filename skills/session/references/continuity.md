# Continuity contract

## Standing record

Open the checkpoint with the typed standing record in its frontmatter. Every
field is required; empty lists are explicit:

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
grants and asks that ended.

Hooks restore the record ahead of the prose at session start, after compaction
and after an account switch; `provenant status` reports what waits on the
owner. Lanes cannot save chair checkpoints, and hooks keep a separate autosave.

## Body

Goal and owner; direction and authority; Intent; evidence pointers; next
actions, blockers and resume trigger. Write PR references as `PR #N`. Link live
lanes and criteria to their owners instead of copying changing status.

The Intent section is the chair's understanding, not quotation. For each live
ruling record what the user wants, the reason as you understand it, rejected
alternatives, open disagreement and any reading you are unsure of. A
correction replaces the earlier reading. Quote the user only where wording
matters: an authority grant, or a correction of a misreading. Hooks show this
section first after compaction and name the host transcript for checking an
entry in doubt; save it at each ruling or correction and before a long task.

Retain rulings, authority, evidence pointers, open questions and the next
action. Drop transcripts, tool output and anything that can be reopened.

## Chair log

For retryable appends keep `--operation-id ID`, `--issued-at UNIX_SECONDS` and
identical JSON input. Pending inbox answers keep their original
host/project/session/actor across handoff; changing a label does not move
their delivery target.
