---
name: tanstack-query
description: "Use for TanStack Query v5 server-state code: keys, options, freshness, mutations, invalidation, optimistic updates, pagination, cancellation, or SSR. Not for generic React state, performance, routers, or backend caches."
---

# TanStack Query

Design TanStack Query as a server-state boundary, not a fetch-hook convenience.
This skill targets `@tanstack/react-query` v5. Confirm the installed major and
framework adapter before changing APIs; use current official documentation for
version-sensitive behaviour.

## Core rules

- Query keys are top-level arrays, JSON-serialisable and complete: include every
  input that can change the returned data. Centralise families without hiding
  their hierarchy.
- Co-locate `queryKey` and `queryFn` with `queryOptions`; reuse that contract for
  hooks, prefetching, invalidation and cache updates.
- Query functions throw/reject on failure and consume the supplied `AbortSignal`
  when the transport supports cancellation.
- Choose `staleTime` from data semantics. Choose `gcTime` from inactive-cache
  retention needs. Do not suppress focus/reconnect refetches to conceal a poor
  freshness policy.
- Prefer targeted invalidation after mutations. Use `setQueryData` only when the
  response is canonical for the exact cached shape.
- Optimistic UI through mutation variables is simpler than cache mutation. If
  updating the cache, cancel matching queries, snapshot, update immutably,
  restore on error and invalidate on settlement.
- Keep initial loading, background fetching, empty data and errors distinct.
  Never blank usable data merely because `isFetching` is true.
- Avoid serial request waterfalls: start independent queries together and
  prefetch at route or intent boundaries where evidence justifies it.
- Create a request-scoped `QueryClient` for SSR. Never share a server cache
  between users. Hydrate only serialisable, authorised data.
- Tests receive a fresh `QueryClient`, disabled retries for error cases and
  network behaviour exercised at the public boundary.

## Workflow

1. Inventory query clients, key families, option factories, mutation effects,
   SSR boundaries and existing tests.
2. State freshness, invalidation, concurrency and error invariants before code.
3. Implement through `implement`, applying `tdd` to behaviour changes; load
   `typescript-clean-code` for type boundaries and `react-performance` only for
   broader React/Next.js performance.
4. Run tests, typecheck and the TanStack Query ESLint plugin where installed.
5. Review with [review-checklist.md](references/review-checklist.md).

Load [patterns.md](references/patterns.md) for implementation patterns and
[sources.md](references/sources.md) for the official source map and provenance.
