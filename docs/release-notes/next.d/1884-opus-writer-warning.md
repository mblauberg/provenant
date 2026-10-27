## Routing

Writer lanes using an Opus route now print a one-line advisory suggesting a
cheaper route in the same pool. Dispatch continues normally. Set
`suppress_opus_writer_warning = true` under `[domains.routing]` in `config.toml`
to silence it.
