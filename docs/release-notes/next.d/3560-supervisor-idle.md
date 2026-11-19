## Fixes

### Lane supervision

- A guardian retaining custody of a detached service no longer burns most of a
  CPU core. It waits on process, cancellation and signal events, repeats its
  bounded stop every 30 s instead of every 20 ms, and reads lane graph
  checkpoints only when it has a process to signal. (#3560)
