## Behaviour changes

### Lanes and settlement

- The global Checks rule now forbids cold builds only in leaf lanes. An integrator working on an integration branch, a large batch or the default branch may build cold when no warm cache exists, instead of waiting on CI. The host's resource limits (cores and free memory) admit the build, not a fixed count. These are defaults that a host or project can replace with its own build, cache and verification policy. (#4141)
