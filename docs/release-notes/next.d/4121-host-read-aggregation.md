---
title: Read and collect from selected hosts
---

`status`, `output`, `result`, `events` and `gc` now accept repeatable `--host` names or `--host all`. Results retain each host's command status and data in one aggregate; human output has a section per host. If one peer cannot be reached, its `Unavailable` status remains the worst outcome, while answered hosts are retained with `partial: true` and a `PEER_UNREACHABLE` warning.

`output --final --to` pages each selected host's sealed result over the peer transport, validates the stable attempt, digest and contiguous offsets, then writes the collected content at the caller's destination. Event consumer cursors advance only after the represented host page is delivered successfully.
