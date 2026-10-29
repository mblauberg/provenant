# Imperative query migration

Version-sensitive guidance last checked 22 October 2026; verify against the installed package.

Query 5.10x deprecates `fetchQuery`, `prefetchQuery`, `ensureQueryData` and
their infinite-query versions, ahead of removal in the next major. Use
`queryClient.query(opts)` and `queryClient.infiniteQuery(opts)`:

- fetch becomes `query(opts)`;
- prefetch becomes `query(opts).catch(noop)`, because a prefetch must not throw;
- ensure becomes `query({ ...opts, staleTime: 'static' })`.

Write the new form in new code. Flag existing call sites for migration rather
than changing them inside unrelated work. If unsure, search `@deprecated` in
the installed `query-core` type declarations. Older docs still show
`ensureQueryData`.

`staleTime: 'static'` overrides invalidation for active observers. Keep it to
one-off imperative calls, never hooks whose data a mutation must refresh.
`Infinity` still honours invalidation.
