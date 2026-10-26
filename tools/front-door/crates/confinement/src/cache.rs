//! Lane cache collection.

pub use crate::permissions::{
    CacheGcEntry, CacheGcError, CacheGcPolicy, CacheGcReason, CacheGcReport,
    gc_worktree_cargo_caches, gc_worktree_cargo_caches_with_writer_guard,
    prune_orphaned_worktree_cargo_caches,
};
