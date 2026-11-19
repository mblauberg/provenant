## Behaviour changes

### Lanes and settlement

- `gc` reclaims settled attempt scratch and stale shared Cargo incremental sessions after `gc.idle`, and reports busy items in its preview. Discard archives exclude build caches and expire after `gc.discard_archive_retention`, which defaults to 14 days; writer lanes warn when their Cargo target is in attempt scratch (read-only lanes build there by design). See the [GC guide](../../runbooks/gc.md). (#3541)
