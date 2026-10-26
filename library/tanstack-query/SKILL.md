---
name: tanstack-query
description: "Use for TanStack Query v5, Router loaders, Table v9 and react-virtual: keys, queryOptions, staleTime, mutations, useTable, virtualised rows. Not for local UI state, render speed (react-performance) or backend caches."
---

# TanStack Query v5, Router, Table and Virtual

This skill targets `@tanstack/react-query` 5.10x, so confirm the installed
version first. Project conventions win over this file.

## API newer than most training data

- **Imperative calls have moved.** Query 5.10x deprecates `fetchQuery`,
  `prefetchQuery`, `ensureQueryData` and their infinite-query versions, ahead
  of removal in the next major. The replacements are `queryClient.query(opts)`
  and `queryClient.infiniteQuery(opts)`:
  - fetch becomes `query(opts)`;
  - prefetch becomes `query(opts).catch(noop)`, because a prefetch must not
    throw;
  - ensure becomes `query({ ...opts, staleTime: 'static' })`.

  Write the new form in new code. Flag old call sites for migration rather
  than rewriting them inside an unrelated change. If unsure, grep `@deprecated`
  in the installed `query-core` type declarations. Older docs still show
  `ensureQueryData`.
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
- **Status flags:**
  - `isPending` means no data yet;
  - `isFetching` means a request is running;
  - `isLoading` means both.

  Keep cached data on screen during a background refetch. When a list
  changes key (new sort or filter), use `placeholderData: keepPreviousData`.
- **Freshness:** the default `staleTime` of 0 refetches on every mount and
  focus. Set it from how fast the data actually changes. Switching off focus
  refetching to hide a bad value is the wrong fix.
- **Tracked results:** destructure only the fields you read. Rest-spreading the
  result, or listing the whole result object as a dependency, breaks property
  tracking.
- **Optimistic updates:** show the mutation's `variables` before editing the
  cache; money is never optimistic. Cache edits need cancel, snapshot,
  rollback and settled invalidation: see
  [references/mutations.md](references/mutations.md).
- **Mutation aftermath:**
  - Invalidate the narrowest key prefix that covers the change.
  - Write the response straight into the cache only when its shape is exactly
    the cached shape.
- **Query data is not component state:** do not copy it into `useState`.
  Derive during render or use `select`.
- **`skipToken`:** it disables a query with type safety, but `refetch()`
  cannot run that query.
- **`useSuspenseQuery`:** no `enabled` or `placeholderData`; a key change
  suspends again unless inside `startTransition`. Details in
  [references/mutations.md](references/mutations.md).

## Router loaders and tables

Loaders await `queryClient.query({ ...opts, staleTime: 'static' })` and the
component calls `useSuspenseQuery(opts)` with the same factory. See
[references/router.md](references/router.md).

Table and virtual rules to always apply:

- Table is headless. Keep the project's markup and feed it from `useTable`.
- Server-driven tables set `manualSorting`, `manualFiltering` and
  `manualPagination`, and hold that state in the query key, never only in the
  table. Pass `getRowId` with the record's stable id.
- Never use `stockFeatures` in shipped code. Register only the features used.
- Virtualise only hundreds of rendered rows. Banned: `rows.map` over the full
  model when thousands are loaded.

## Verification

- Table tests render the component and assert on rows, header sort state and
  selection, not on `table` internals.
- Tests get a fresh `QueryClient` with `retry: false` and go through the public
  hook or component.
- `@tanstack/eslint-plugin-query` provides `exhaustive-deps`,
  `stable-query-client`, `no-rest-destructuring`, `no-unstable-deps` and
  `prefer-query-options`. Suggest it where it is missing; don't add it unasked.
