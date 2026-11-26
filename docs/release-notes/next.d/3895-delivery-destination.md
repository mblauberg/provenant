## Fixes

### Delivery

- A delivery destination is now typed: `Local` or `Remote(<name>)`, parsed once with the same `RemoteName` grammar the forge resolver uses. A resolved remote such as `team/upstream` can now be delivered instead of being refused by a second, narrower alphabet in the write path, and the read and write paths validate identically. The empty remote string is gone from delivery requests, launch bindings and receipts: a local integration records a local destination, and an old record whose remote is `""` is refused by strict decoding rather than being silently read as "no remote". (#3895)
