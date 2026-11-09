## Fixes
### Lanes and settlement

- A lane that ends a turn with a valid handoff and then only replies to a late background notification now settles with that handoff instead of `review_required` with `handoff_missing` (#3115). Claude turns now start on their own line in the result, and settlement reads the handoff that ended a turn: a later turn's own fence replaces it, and later tool work without one leaves no handoff. A fence quoted mid-turn is still never final. Review verdicts keep their final-position rule.
