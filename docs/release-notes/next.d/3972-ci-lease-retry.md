### Fixed

- Delivery queue CI leases can be reacquired after release and uncapped requests can start without an existing waiter directory. (#3972)
- Parked delivery transitions persist their updated wake time for restart recovery. (#3972)
- The graph checkpoint envelope accommodates full delivery handoffs at the default node limit. (#3972)
