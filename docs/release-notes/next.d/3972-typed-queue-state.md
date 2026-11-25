## Behaviour changes
### Pipelines and graphs

- Queue control and CI state now use declared record fields. Malformed persisted queue evidence is reported as invalid instead of being silently ignored; unknown extension fields continue to survive round trips. (#3972)
