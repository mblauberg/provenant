## Behaviour changes

### Pipelines and graphs

- `provenant steer` now saves messages durably, waits for the shared adapter decoder to validate the live session, then requests an interrupt and resumes that same session after settlement. Lane status shows steering state and stable operation/message IDs without exposing the text. The guardian preserves the typed steering stop and seals live identity conflicts independently of retained stdout; incomplete live identity evidence remains unsettled. Identity disagreement or a competing stop keeps the saved text and prevents a fresh-session fallback. (#3318)
