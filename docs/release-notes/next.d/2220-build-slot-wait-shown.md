## Additions
### Lanes and settlement

- `lanes` and `status` now show a running lane whose build waits for a host build slot, or for another build's Cargo lock, as `build slot: ...` in `waiting_for`, the way they show admission waits. The `cargo` shim publishes the wait, one file per waiting command, in the lane's writable scratch directory and the host reads it back without trusting its contents beyond one of two fixed kinds and its age (#2220).
