---
name: tanstack-query
description: "Use for TanStack Query v5 code: keys, queryOptions, staleTime, mutations, invalidation, optimistic updates, router loaders. Not for local UI state, render speed (react-performance) or backend caches."
---

# TanStack Query v5

This skill targets `@tanstack/react-query` 5.10x, so confirm the installed
version first. Project conventions win over this file.

## API newer than most training data

- **Imperative calls have moved.** Late 5.x deprecates `fetchQuery`,
  `prefetchQuery`, `ensureQueryData` and their infinite-query versions, ahead
  of removal in the next major. The replacements are `queryClient.query(opts)`
  and `queryClient.infiniteQuery(opts)`:
  - fetch becomes `query(opts)`;
  - prefetch becomes `query(opts).catch(noop)`, because a prefetch must not
    throw;
  - ensure becomes `query({ ...opts, staleTime: 'static' })`.

  Write the new form in new code. Flag old call sites for migration rather
  than rewriting them inside an unrelated change. If unsure, grep `@deprecated`
  in the installed `query-core` type declarations.
- **`staleTime: 'static'` overrides invalidation.** If any active observer of
  a query uses it, `invalidateQueries` and `refetchQueries` skip that query.
  Keep it to one-off imperative calls, and never put it on a hook whose data a
  mutation must refresh. `Infinity` still honours invalidation.

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

  Keep cached data on screen during a background refetch.
- **Freshness:** the default `staleTime` of 0 refetches on every mount and
  focus. Set it from how fast the data actually changes. Switching off focus
  refetching to hide a bad value is the wrong fix.
- **Tracked results:** destructure only the fields you read. Rest-spreading the
  result, or listing the whole result object as a dependency, breaks property
  tracking.
- **Optimistic updates:**
  - Display the pending mutation's `variables` (or `useMutationState` from
    another component) before touching the cache.
  - If several views must move together and you do edit the cache, stop
    in-flight fetches for those keys and keep the previous value. Apply the
    change without mutating, put the saved value back if the request fails,
    and invalidate once it settles.
  - Have `onSettled` hand back the invalidation promise, so the mutation reads
    as pending until the refetch lands.
  - Money is never optimistic.
- **Mutation aftermath:**
  - Invalidate the narrowest key prefix that covers the change.
  - Write the response straight into the cache only when its shape is exactly
    the cached shape.
- **`skipToken`:** it disables a query with type safety, but `refetch()`
  cannot run that query.
- **`useSuspenseQuery`:**
  - It has no `enabled` or `placeholderData`.
  - A key change suspends again unless it happens inside `startTransition`.
  - Errors hit the boundary only when no cached data exists.

## TanStack Router loaders

Put `queryClient` in router context. In the loader, await
`queryClient.query({ ...opts, staleTime: 'static' })` for data the route cannot
render without. Start the rest without awaiting, using `.catch(noop)`. The
component calls `useSuspenseQuery(opts)` with the same factory, so loader and
component always agree on the key.

## Verification

- Tests get a fresh `QueryClient` with `retry: false` and go through the public
  hook or component.
- `@tanstack/eslint-plugin-query` provides `exhaustive-deps`,
  `stable-query-client`, `no-rest-destructuring`, `no-unstable-deps` and
  `prefer-query-options`. Suggest it where it is missing; don't add it unasked.

## Sources

Checked 22 October 2026: tanstack.com/query/latest (QueryClient reference,
optimistic updates, Suspense, v5 migration, ESLint plugin),
tanstack.com/router/latest external data loading, TanStack/query GitHub
releases, and the `@tanstack/query-core` 5.104.2 declarations and source.
