# Router loaders with Query

Put `queryClient` in router context. In the loader, await
`queryClient.query({ ...opts, staleTime: 'static' })` for data the route cannot
render without. Start the rest without awaiting, using `.catch(noop)`. The
component calls `useSuspenseQuery(opts)` with the same factory, so loader and
component always agree on the key.

- Validate search params with a schema (`validateSearch`) and return only the
  parts the loader reads from `loaderDeps`, so unrelated search keys do not
  reload the route. Table sort, filter and cursor state belongs in search
  params when it should survive reload and sharing.
- Set `defaultPreloadStaleTime: 0` when Query owns freshness, so the router
  does not add a second freshness layer.
- Parallel suspense queries waterfall. Use `useSuspenseQueries` or start them
  in the loader.
- Use `infiniteQueryOptions` like `queryOptions`, with `initialPageParam` and
  a keyset `getNextPageParam`, not an offset.
- Order keys general to specific (`['clients', 'list', filters]`) so a prefix
  invalidates a family.
- Put cross-cutting mutation side effects in `mutationOptions` factories or
  `mutationKey` defaults, not at each call site.
