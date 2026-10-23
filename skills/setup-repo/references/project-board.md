# Project board with standard statuses

Extracted from provenant's live Project 2 (`gh project field-list 2 --owner
mblauberg`) and the commands its runbook uses to move items.

## Create the board

```sh
gh project create --owner <owner> --title "<repo> work"
# -> note the returned project NUMBER and PVT_... id
```

## Set the Status field's options

A new `gh project create` ships a default `Status` single-select field with
generic options (Todo/In Progress/Done). Replace the option set with the six
statuses this skill's doctrine uses (see
[doctrine.md](doctrine.md#statuses-project-board)):

```sh
gh project field-list <number> --owner <owner> --format json \
  --jq '.fields[] | select(.name == "Status")'
# -> note the field id (PVTSSF_...) and each existing option id
```

`gh project field-list`/`field-edit` do not expose a clean "replace options"
verb; the reliable path is the GitHub UI (Project → ⋯ → Settings → Status
field → edit each option's name, add/remove until exactly six remain: Backlog,
Ready, In progress, In review, Awaiting user, Done) or the `updateProjectV2Field`
GraphQL mutation via `gh api graphql` if the UI is not available — note the
mutation is `updateProjectV2Field` with a `singleSelectOptions` argument, not
`updateProjectV2SingleSelectField` (that type name exists only as the option
input, not as a mutation field):

```sh
gh api graphql -f query='
mutation {
  updateProjectV2Field(input: {
    fieldId: "<status_field_id>",
    singleSelectOptions: [
      {name: "Backlog", color: GRAY, description: "Untriaged or explicitly deferred."},
      {name: "Ready", color: BLUE, description: "Accepted with bounded scope, authority and acceptance evidence."},
      {name: "In progress", color: YELLOW, description: "An owner is executing the accepted work."},
      {name: "In review", color: ORANGE, description: "The pull request, checks or independent review is active."},
      {name: "Awaiting user", color: PURPLE, description: "A user decision remains, including explicit acceptance for terminal work or approved scope."},
      {name: "Done", color: GREEN, description: "The item is integrated, or closed with its terminal reason recorded."}
    ]
  }) {
    projectV2Field {
      ... on ProjectV2SingleSelectField { id name options { id name } }
    }
  }
}'
```

`description` is a required (non-empty-allowed) string on each option; omitting
it errors. Either way, re-run `field-list` afterwards and record the resulting
option ids — the runbook's day-to-day commands need them.

## Moving an item (for the copied runbook)

```sh
item=$(gh project item-list <number> --owner <owner> --limit 200 --format json \
  --jq '.items[] | select(.content.number == <n>) | .id')
gh project item-edit --project-id <PVT_id> --id "$item" \
  --field-id <status_field_id> --single-select-option-id <option_id>
```

Record the project number, `PVT_` id, `Status` field id and each option id in
the target project's own runbook once created — they are stable but
per-project, and re-deriving them via `field-list` is the fallback if an id
stops matching after an edit.
