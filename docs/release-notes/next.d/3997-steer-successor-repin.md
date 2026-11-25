## Fixes
### Fabric
- A steer successor now re-pins automatically when the lane's retained writer committed after its predecessor was sealed: it adopts the descendant head, keeps the original source pin and warns with both ids. A head that moved under different custody stays a typed continuation refusal, leaving the graph needing retry rather than failed, and a route without a resumable session still continues through `fresh_with_handoff`. (#3997)
