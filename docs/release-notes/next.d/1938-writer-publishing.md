## Additions

Writer lanes with open network can push their own branch to origin and manage
pull requests using the existing gh login. A pre-push hook refuses protected
branches, tags, deletions and forced rewinds. Merges use the existing exact-head
CI gate. Set `lanes.publish=false` to keep publishing with the chair.
