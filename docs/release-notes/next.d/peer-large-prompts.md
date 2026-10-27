## Fixes

Peer dispatch accepts prompts over 64 KiB within the launch and encoded peer
frame limits. Large argv prompts are sealed on the peer, and rerunning with
the same operation ID recovers the lane's outcome. Requests whose encoded
frame exceeds 1 MiB are refused before submission with a size error.
