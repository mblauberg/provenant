## Behaviour changes

### Accounts, quota and routing

- Recorded refusals are advisory. A model that is not in the catalogue was already attempted with a warning; now the provider's own refusal for a nonexistent model is classified like any other model-unavailable outcome, without an extra "nearest catalogued IDs" note (#3551)
- An explicit model pin whose recorded quota or pacing state says exhausted is now attempted with a warning naming the record's source and expiry, because recorded refusals go stale. Unpinned routing still prefers available routes (#3551)
- `providers.<adapter>.out_of_scope_pin = refuse` no longer stops a launch. An out-of-scope explicit pin warns with its setting source and is attempted (#3551)
