## Fixes

### Lanes and settlement

- Steering uses a separate stop intent, so a steer is never mistaken for cancellation. While ownership or placement is being confirmed, the graph retains the instructions and retries with a warning. (#3336)
