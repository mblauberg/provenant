## Fixes
### Git and safety

- `land` uses paced REST polling and retries GitHub rate limits within the wait budget. REST merge commits use the GitHub account's commit email rather than the local Git author email (#2757).
