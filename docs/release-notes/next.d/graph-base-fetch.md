## Fixes
### Pipelines and graphs

- A graph or lane with a remote-tracking base such as `origin/main` no longer fails after the remote moves past the last fetch. Provenant fetches that branch once, and a graph refused for a base it still cannot resolve now names the base instead of reporting `invalid input`.
