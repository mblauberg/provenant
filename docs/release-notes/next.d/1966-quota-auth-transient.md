## Fixes

- Treat transient Claude credential and identity read failures as unknown and retry with a growing delay from 15 to 300 seconds. Reserve authentication failures for explicit rejections or missing credentials, and allow manual quota checks to retry accounts in authentication backoff.
