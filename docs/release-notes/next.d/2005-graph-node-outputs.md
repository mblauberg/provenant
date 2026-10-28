## Additions

- Graph nodes record typed, bounded outputs and can select outputs from direct
  dependencies for their briefs. Graph status JSON exposes SHA-256 result
  digests and bounded excerpts. Selected outputs are delimited as untrusted data.
- The digest identifies the raw stored result; excerpts and output reads are
  statefully credential-redacted and may not hash to that digest.
- Result paths are visible in graph status and omitted from downstream briefs.
