# System-design patterns: topology & framework choice
Status: Archived research note

Evidence snapshot: July 2026

For **designing a multi-agent system you are building**: the standing product/architecture, not this
run's fan-out. The `orchestrate` skill's references carry the runtime doctrine (dispatch, reduce, gate);
read this archived note when you must pick a topology or framework and commit it to code. Current
context-isolation, handoff, routing, worker-cap, and panel contracts are owned by the `orchestrate`
skill and are not repeated here.

## Workflows before agents

Confirm you need agents at all. **Workflows** orchestrate LLM calls through predefined code paths:
predictable, debuggable, cheap. **Agents** let the model direct its own process and tools: flexible,
less predictable, costlier. Most production needs are met by one agent or one workflow pattern; reserve
true multi-agent systems for high-value, hard-to-decompose work. The five composable workflow patterns,
tried before reaching for multiple agents:

- **Prompt chaining**: sequential calls, each on the prior output, with gates between. Fixed, ordered subtasks.
- **Routing**: classify input, dispatch to a specialised handler. Distinct input categories.
- **Parallelisation**: concurrent calls; *sectioning* (independent subtasks) or *voting* (same task N times for confidence).
- **Orchestrator-workers**: a central LLM decomposes, delegates, synthesises. Subtasks can't be enumerated up front. (The pattern most systems that think they need "multi-agent" actually need.)
- **Evaluator-optimiser**: one generates, another critiques, in a loop. Clear eval criteria + iterative refinement pays.

## Topology trade-offs (choose by coordination need, not org metaphor)

| Topology | Use when | Cost |
|---|---|---|
| **Orchestrator-workers (supervisor)** | central control, user oversight, decomposable tasks | supervisor context is a bottleneck; its failure cascades; paraphrase/"telephone" loss |
| **Handoff (peer routing)** | can't know which specialist is needed until the conversation unfolds | no central state-keeper; divergence risk |
| **Peer network (decentralised)** | breadth-first exploration; rigid planning counterproductive | coordination cost grows with agent count; needs convergence constraints |
| **Hierarchical (strategy/plan/execution)** | large projects with genuine management layers | inter-layer overhead; strategy-execution misalignment |

Production systems nest these: handoff routing on top, a hierarchical manager per team, workflow
patterns inside each specialist. Cap workers per supervisor at 3–5; add a tier rather than overload one.

## The telephone-game problem

Naive supervisors paraphrase sub-agent responses and lose fidelity each pass, materially worse than
the optimised version. Fix: a `forward_message` tool letting a sub-agent's final, complete response pass
**directly** to the user without supervisor re-synthesis. Use it when synthesis would only lose detail or
break required formatting. Prefer peer-handoff over supervisor when sub-agents can answer users directly.
(At the state level, the same failure is fixed by filesystem coordination over message-passing: see
the `orchestrate` skill's layering-and-context reference.)

## Token economics

Budget for an order-of-magnitude jump. Anthropic's research system ran ~15× a single-agent chat:

| Architecture | Multiplier | Use |
|---|---|---|
| Single-agent chat | 1× | simple queries |
| Single agent + tools | ~4× | tool-using tasks |
| Multi-agent system | ~15× | complex research/coordination |

Treat these as a reference point from one published system, not constants: measure your own multiplier.
BrowseComp analysis found token usage, tool-call count, and model choice explain ~95% of performance
variance (token usage alone ~80%), which is why distributing work across separate context windows helps.
But upgrading the model often beats doubling the token budget: pair a stronger orchestrator with cheaper
workers.

## Framework picker

Frameworks speed early development but hide the underlying prompts/responses: understand the abstraction
before depending on it; for simple systems hand-rolled code often wins.

- **LangGraph**: graph state machines, explicit nodes/edges, checkpointing with time-travel. Fine-grained control, you design the workflow.
- **OpenAI Agents SDK**: production successor to Swarm; primitives = agents, agents-as-tools, handoffs, guardrails, sessions, tracing, agent loop. Best for handoff/routing + governance.
- **OpenAI Swarm**: archived educational (routines + handoffs); read for the handoff primitive, don't build on it.
- **AutoGen / AG2**: conversational, event-driven GroupChat. Debate/conversation topologies.
- **CrewAI**: role-based hierarchical crews. Opinionated role-first designs.
- **Native subagents (Claude Code / Codex)**: when orchestrating inside an agent session rather than shipping a product, these already give context-isolated fan-out; use the `orchestrate` skill's runtime doctrine instead of standing up a framework.
