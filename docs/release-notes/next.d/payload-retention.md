## Additions

### Lanes and settlement

- `runs/` now stays under a byte bound. `retention.payload_cap_bytes` (default 16 GiB) caps bulk payload; over it, `gc` evicts the oldest eligible provider state, history, scratch and capture bytes until 75% of the cap. Payload under 24 hours old, or that holds unique private Git objects, an abandonment, a reservation or a continuation, is never evicted. Records, results, exits, receipts and capture tails stay, and `output` and `status` report the payload as evicted. `gc` and `disk report` show pinned and reclaimed bytes.
