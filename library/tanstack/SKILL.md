---
name: tanstack
description: "Use for TanStack Query v5, Router, Table v9 and react-virtual: keys, queryOptions, staleTime, mutations, loaders, search params, useTable, virtualised rows. Not for local UI state, render speed (react-performance) or backend caches."
---

# TanStack Query, Router, Table, Virtual

This skill targets `@tanstack/react-query` 5.10x, so confirm the installed
version first. Project conventions win over this file.

## API newer than most training data

- **Imperative calls:** Query 5.10x deprecates fetch/prefetch/ensure methods.
  Read [migration mappings](references/imperative-queries.md) before writing
  calls. Use the new form in new code; flag existing sites rather than
  migrating them inside unrelated changes. Prefetch must never throw.
- **`staleTime: 'static'` overrides invalidation.** If any active observer of
  a query uses it, `invalidateQueries` and `refetchQueries` skip that query.
  Keep it to one-off imperative calls, and never put it on a hook whose data a
  mutation must refresh. `Infinity` still honours invalidation.
- **Table v9 is not v8.** `useReactTable` is now `useTable`, features go
  through `tableFeatures()`, and `getState()` became `table.state`. Read
  [references/table.md](references/table.md) before writing table code.

## Where agents slip

- **Incomplete keys:** the key must hold every value the `queryFn` reads.
  Otherwise one tenant's or filter's data is served under another's key.
- **Duplicated query contracts:** write one `queryOptions` factory per resource
  and reuse it in the hook, the loader, prefetching and
  `getQueryData`/`setQueryData`. The typed key then carries the data type.
- **Failed requests that resolve:** `fetch` resolves on 4xx and 5xx responses.
  The `queryFn` must throw for them, and should forward `signal` so requests
  can be cancelled.
- **Status:** `isPending` means no data; `isFetching` means a running request;
  `isLoading` means both. Keep cached data during refetches; list key changes
  use `placeholderData: keepPreviousData`.
- **Freshness:** choose `staleTime` from data freshness needs. Its default 0
  refetches on mount/focus; never disable focus refetching to hide a bad value.
- **Tracked results:** destructure only fields read; never rest-spread results
  or use the whole result as a dependency.
- **Optimistic updates:** show the mutation's `variables` before editing the
  cache; money is never optimistic. Cache edits need cancel, snapshot,
  rollback and settled invalidation: see
  [references/mutations.md](references/mutations.md).
- **Mutation aftermath:** invalidate the narrowest covering key prefix.
  Cache responses only with exactly the cached shape.
- **Query data is not component state:** do not copy it into `useState`.
  Derive during render or use `select`.
- **`skipToken`:** it disables a query with type safety, but `refetch()`
  cannot run that query.
- **`useSuspenseQuery`:** no `enabled` or `placeholderData`; a key change
  suspends again unless inside `startTransition`. Details in
  [references/mutations.md](references/mutations.md).

## Router loaders and tables

Loaders and components share a query factory. Read
[router integration](references/router.md) before loader work.

Table and virtual rules to always apply:

- Preserve project markup; feed headless tables through `useTable`.
- Server-driven tables set `manualSorting`, `manualFiltering` and
  `manualPagination`, and hold that state in the query key, never only in the
  table. Pass `getRowId` with the record's stable id.
- Never use `stockFeatures` in shipped code. Register only the features used.
- Virtualise only hundreds of rendered rows. Banned: `rows.map` over the full
  model when thousands are loaded.

## Verification

Test public hooks/components with a fresh `QueryClient` and `retry: false`.
Table assertions cover rendered rows, header sort and selection, never internals.
Suggest missing Query ESLint rules; never add them unasked. See
[verification](references/verification.md).
