## Behaviour changes

### Lanes and settlement

- Lane instructions treat a write scope as the expected files, not a fence. A writer whose fix needs another file edits it minimally and lists it in its handoff instead of stopping as blocked; only a strict `WRITE_SCOPE` lane policy refuses those paths at settlement, #3354.
