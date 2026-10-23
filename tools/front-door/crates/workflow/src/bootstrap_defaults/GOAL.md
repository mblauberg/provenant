# GOAL — the north star + the RUN/STOP gate
<!-- HUMAN-OWNED. The conductor reads this at the START of every iteration
     and obeys it. It must NEVER author content into this file. The ONLY
     clean exit is a human setting STATUS: STOP. -->

```config-knobs
DOMAIN            = {{DOMAIN}}
MISSION           = {{MISSION}}
LOCKED_CONSTRAINTS = {{LOCKED_CONSTRAINTS}}
BUILD_CEILING     = {{BUILD_CEILING}}
ESCALATION_GATES  = {{ESCALATION_GATES}}
RUNAWAY_CAPS      = {{RUNAWAY_CAPS}}
```

## Mission

{{MISSION}}

Never self-close the mission. An empty queue triggers one bounded
re-enumeration pass; if still dry, write an idle checkpoint and pause until
a material resume trigger. Only human STOP closes the mission.

## Active directives
<!-- Human-editable steering block. Empty = follow default traversal. -->

## STATUS gate

STATUS: RUN
<!-- audit note: who/why/when this last flipped. RUN at scaffold. -->
PREV: (none)
