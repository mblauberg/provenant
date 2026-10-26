# Mutations and Suspense

## Optimistic updates

- Display the pending mutation's `variables` (or `useMutationState` from
  another component) before touching the cache.
- If several views must move together and you do edit the cache, stop
  in-flight fetches for those keys and keep the previous value. Apply the
  change without mutating, put the saved value back if the request fails,
  and invalidate once it settles.
- Have `onSettled` hand back the invalidation promise, so the mutation reads
  as pending until the refetch lands.
- Money is never optimistic.

## useSuspenseQuery

- It has no `enabled` or `placeholderData`.
- A key change suspends again unless it happens inside `startTransition`.
- Errors hit the boundary only when no cached data exists.
