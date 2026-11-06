# Upgrade drain fixture

`old-pending-checkpoint.json` is hand-built from `GraphCheckpoint`,
`NodeProgress`, `GraphDocument` and `GraphExecution` in
`git show origin/main:tools/front-door/crates/fabric-runtime/src/graph/model.rs`
and `StoredCheckpoint` in the neighbouring `graph/journal.rs` (main at
684d73c5f). It represents a pending reader before the template graph cutover.
All names, paths and digests are synthetic; no captured state is included.
The envelope digest uses the old `fabric-graph-checkpoint` version 2 domain
and the canonical JSON tuple `[checkpoint, execution]`. The plan digest is a
synthetic placeholder: the journal does not validate it against a compiled plan.
Keep this fixture independent of the current graph serializer.
