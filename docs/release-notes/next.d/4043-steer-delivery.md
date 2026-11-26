## Behaviour changes

### Pipelines and graphs

- `provenant steer` queues its text for the next turn and leaves a running lane alone by default. `domains.fabric.steer.delivery = "queued" | "now"` sets the host default (`queued`), and `provenant steer --explain` reports the effective mode and the layer that set it. The new `--now` flag asks for the session-preserving restart: once the text is durable and a validated provider session is bound to the exact route, custody requests a `Steered` stop and settlement resumes that same session with the steering text. `--now` on a lane without a retained session is refused as `steer_session_unavailable` and leaves the attempt running; an interrupt path that could strand the accepted text no longer runs on the default route. (#4043)
