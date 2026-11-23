## Fixes
### Lanes and settlement

- Writer settlement checks for an in-progress Git operation in every directory
  the lane writes — its private Git and its candidate common directory — before
  it imports objects or publishes the branch. A lane's own unfinished operation
  warns, names its commit and retained directory, and publishes the committed
  tip without resetting it; only a host checkout left mid-operation refuses,
  before import, with its bytes unchanged. (#3407)
