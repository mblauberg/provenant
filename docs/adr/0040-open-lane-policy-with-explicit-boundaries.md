# ADR 0040: Ambient lane access with explicit mandatory boundaries

**Status:** Accepted

## Context

A personal harness should let an agent work with ordinary host access while protecting real authority and custody boundaries. The same launch policy applies to child and direct launches.

## Decision

Ordinary filesystem, process and network access is ambient unless a mandatory boundary, an explicit lane request, an adapter offline requirement or optional host policy restricts it. Credentials, private state, source writes and shared Git remain subject to their applicable boundaries ([ADR 0035](0035-four-hard-boundaries-and-flexible-everything-else.md)). Exceptions for selected authentication, GitHub access, attempt-private state, installation assets and source access are explicit and bounded.

## Sandbox mode

The host may select sandboxed or trusted unsandboxed routes. Unsandboxed routes rely on route trust and delivery checks rather than kernel denial for shared Git writes. The [configuration runbook](../runbooks/configuration.md#lane-sandbox-mode) owns operation; the [supervisor spec](../specs/supervisor.md#permission-policy) owns the permission contract.

## Permitted exceptions

Exceptions grant only the access needed for their declared purpose and do not transfer credentials between hosts.

## Consequences

Ambient access avoids unnecessary refusals but makes route trust and explicit authority consequential. Revisit this decision if a protected boundary cannot be enforced or its exception cannot be bounded.
