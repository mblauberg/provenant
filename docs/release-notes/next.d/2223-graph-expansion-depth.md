## Behaviour changes
### Pipelines and graphs

- **The default host graph `expansion_depth` is now 32, up from 4.** The bundled `big-batch` preset has a dependency chain of 29, so it now runs on a default host; composite nesting counts against the same limit, and a graph document cannot raise it (#2223).
