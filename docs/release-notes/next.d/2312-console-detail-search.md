## Additions

`provenant console detail` reads one attempt's routes, usage, trace, timings and artifact sizes and presence. `provenant console search` searches local prompt or result text with bounded scanning, snippets and snapshot-bound cursors. It returns the first match per artifact and rescans on each cursor page; peers are not searched. Both are available through the discovered `fabric_console` tool.

The performance regression test uses p95 limits of 2 seconds for 100 detail reads and 6.5 seconds for a 1,000-attempt search. These generous limits differ from the issue's 1 second board reference; they are approximately three times the measured 0.64 and 2.14 seconds on the repair fixture.
