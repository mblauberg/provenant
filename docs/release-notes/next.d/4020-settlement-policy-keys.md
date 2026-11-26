## Behaviour changes

### Lanes and settlement

- Settlement is configurable per signal under `lanes.settlement.*` (`declaration_missing`, `declaration_malformed`, `empty_result`, `uncommitted_work`, `stray_work`, `no_work`). Each key layers default, host, project and route (`model_policy.defaults.lanes.settlement.<key>` or a rule's `policy.lanes.settlement.<key>`), and a document explain shows each winning value, its layer and the key's allowed values. A value outside a key's set warns `SETTLEMENT_POLICY_INVALID` and keeps the next layer. (#4020)
- A route whose observed provider identity contradicts the requested route now decides inside the one classifier: a writer's moved head is kept for review, never delivered or released, and every other lane fails with `identity_mismatch`. (#4020)
