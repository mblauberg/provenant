## Fixes

- Transient Claude credential and identity read failures are now treated as unknown and retried with a delay that grows from 15 to 300 seconds. Authentication failures are reserved for explicit rejections or missing credentials, and a manual quota check can retry an account in authentication backoff.
