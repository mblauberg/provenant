Remote peer readers now bind source and review range to the peer checkout's
`HEAD` and first parent. A sender's scratch commit that the peer has not fetched
no longer prevents the reader from seeing its checkout. When Git evidence is
unavailable, the peer warns and falls back to the sender's pins.
