# 4108 — blocked review lenses are reported as coverage gaps

A review lens whose environment cannot be built used to end the graph as
`blocked_environment`, retrying once on its route and then pausing for the
chair. The panel and its posting steps never ran, even when the other lenses
had finished.

The lens is now recorded as unavailable evidence: its sealed stop is fenced so
no retry or pause sinks the panel, its cause and handoff note name the gap in
the panel result and warnings, and the panel settles on `min_successful` with
the lenses that did finish. A panel whose only gap is a named blocked
environment keeps its complete findings and an inconclusive verdict, so a
delivery review can post the gap report. Blocking findings and hard coverage
floors still refuse.
