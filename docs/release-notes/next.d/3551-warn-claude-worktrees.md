## Behaviour changes

### Git and safety

- The Claude `WorktreeCreate` and `WorktreeRemove` hooks now adopt a clean, idle worktree a person created with a warning naming the difference, instead of refusing it for an off-path location or a missing hook ownership record (#3551)
- A dirty worktree, a tree with a live writer, and the primary checkout are still refused by the hooks and by the owner's non-forcing removal path (#3551)
