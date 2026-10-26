# ADR 0021: Configured-workspace access and one execution owner

**Status:** Accepted

Changed: 2026-10-26, #1754.

## Context

Ordinary configured-provider work needs a clear permission boundary without
mistaking route diversity for assurance or a successful attempt for accepted
delivery.

## Decision

An authorised chair may send ordinary workspace work to any configured provider
family, within the approved path, write and resource scope. Exclude credential
and authentication stores, secrets, unrelated paths and explicitly denied
content. Platform rules and external-action gates still apply. Delegation can
narrow authority, never widen it.

Execution permission and assurance are distinct. Ordinary work may use one
family or mixed workers. An independence claim needs its own verified route
and evidence; provider names or agreement between workers do not prove it.

There is one execution stack: routing resolves routes, confinement compiles
access plans, providers prepares native launches, supervisor owns processes,
and run owns attempts and terminal evidence. CLI and Fabric MCP compose those
owners. Neither creates another adapter parser, scheduler or lifecycle store.

A provider receives only the configuration and credentials admitted by its
launch plan. Codex uses a private home and clean configuration; host hooks,
MCP processes or personal settings must not silently widen a lane's access.
The retired Python endpoint and inherited-config modes are not current launch
contracts. Wire forms belong to the providers crate.

Attempts retain actual route lineage, observed exit and output paths once in
the run store. They may be partial or failed. Workflow's delivery receipt
separately binds verification, review and acceptance, and may refer to those
attempts without duplicating their transcripts.

## Consequences

Cheap ordinary batches remain useful without claiming independent assurance.
One launch owner keeps CLI and MCP behaviour aligned. Launch plans enforce
workspace access while delivery evidence retains a distinct acceptance owner.
