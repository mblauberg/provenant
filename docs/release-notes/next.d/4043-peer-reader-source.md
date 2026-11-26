## Behaviour changes

Remote peer readers now read the initiator's pinned `task.head`, the commit the graph's subject pinned at admission, instead of the peer checkout's own `HEAD` as in 2.10.0, and the review range comes from the pinned subject's base and head. A peer reads the commit the initiator admitted even when its own checkout is stale or has since moved. `lanes.read_source=live` is pinned on a peer and warns `READ_SOURCE_PINNED_FOR_PEER`. (#4043)
