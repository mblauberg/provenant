## Fixes

### Git and safety

- Writer delivery failures now carry a typed cause beside their stage: a failing `git` or `gh` step reports its exit status and a bounded tail of its standard error, while I/O and parse failures carry their own detail. The cause is rendered in the error, the failed delivery receipt and the JSON error envelope, so agents can tell a 422, an authentication refusal, a network failure or a rate limit apart without inspecting the host. Redaction remains off by default by owner decision, so the stderr tail is passed through unchanged
