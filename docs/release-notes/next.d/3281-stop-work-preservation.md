## Fixes

### Lanes and settlement

- Wall deadlines and explicit cancellation preserve commits already made on the lane branch and report the head, remaining dirty paths, and push outcome. They leave dirty files uncommitted; token-budget stops keep their existing safe snapshot commit. (#3281)
