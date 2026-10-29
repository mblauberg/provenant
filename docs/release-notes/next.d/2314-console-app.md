## Additions
### Chair, inbox and console

- A native console app opens an inbox window over the shared Swift client. It lists pending decisions, assumptions and questions, answers them with `--expect-revision` on the `console` channel, and refreshes on window focus and on request. `provenant console open` opens it on macOS and gives an install hint when it is absent (#2314).
