## Behaviour changes

### Lanes and settlement

- `gc` reclaims settled attempt scratch and stale shared Cargo incremental sessions after `gc.idle`, and reports busy items in its preview. Discard archives exclude build caches and expire after `gc.discard_archive_retention`, which defaults to 14 days; a writer lane warns when its Cargo target is in attempt scratch. See the [GC guide](../../runbooks/gc.md). (#3541)
