## Fixes
### Upgrade and stored state

- Attempt records written by v2.8.2 decode again after upgrade. A writable open
  of the run store now runs the base-13-to-14 migration once, which rewrites the
  stored records the provenance and usage renames made unreadable: flat
  provider-usage buckets move under `tokens` with their numbers preserved and
  the free-form source maps to the typed `stream` source; `route_policy` becomes
  `route`, a sandbox `restriction` or `platform` becomes a `constraint` beside
  the `host` layer and a `knob` becomes `host`, and resource-fact,
  setting-origin and port-binding spellings become their shared layer names.
  Without this, status, resume and settlement failed to decode those rows. A
  malformed row is skipped and still fails only its own decode; a row already in
  the new shape is left byte-identical. No decoder accepts a retired spelling.
  (#3849, #3777, #3709)
