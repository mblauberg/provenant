## Fixes
### Git and safety

- Lanes can reach and push to the remote on a host using Apple's `git`, using the lane's own GitHub token. A disk refusal no longer claims uncommitted edits, and one sample of uninterruptible sleep no longer marks a lane stalled.
- Adding lane GitHub credentials preserves inherited Git configuration, including author identity, signing and hooks.
