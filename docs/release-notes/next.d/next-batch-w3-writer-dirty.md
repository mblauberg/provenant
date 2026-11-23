## Changes

### Writer launch

- A writer launched on an unheld worktree with uncommitted changes commits them as WIP and proceeds, by default; the adoption is reported with `WORKTREE_ADOPTED_DIRTY` naming the commit. A worktree another lane holds is still refused, and an explicit `lanes.writer_dirty = refuse` still refuses. (#3649)
