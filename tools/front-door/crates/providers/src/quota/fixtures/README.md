# Quota decoder recordings

The live recordings below were collected read only on 2026-10-24 and redacted
before being added. Credential material, emails and account identifiers are
not retained. The Agy recording keeps a stable redacted Google fingerprint for
the account-attribution regression check.

| Fixture | Provenance |
| --- | --- |
| `claude.json` | Live Claude OAuth usage response. |
| `agy.json` | Live `agy -p /usage --output-format json` response. |
| `cursor.json` | Live Cursor usage-summary response. |
| `codex.json` | Live Codex usage response via the direct WHAM endpoint, used as a read-only fallback to the app-server probe. |
| `opencode.json` | Live local ledger aggregate. It is a partial, this-machine-only estimate and does not include usage from other hosts. |
| `codex-app-server.json` | Synthetic supplemental schema example for app-server response decoding; not a live recording. |

`activation.json` enables quota sources with live redacted recordings. The
synthetic Codex app-server example supplements coverage but is not activation
evidence.
