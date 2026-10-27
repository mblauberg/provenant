# TanStack Pacer (beta)

The docs mark the API as beta and subject to change. Checked against
`@tanstack/react-pacer` 0.24.1 declarations.

- Pick by intent: debounce (latest call after a pause), throttle (evenly
  spaced), rate limit (cap per window), queue (run every call in order), batch.
  Async variants await results and take error and retry options.
- `useDebouncedCallback` and `useDebouncedValue` are the simple forms; use
  `useDebouncer` when you need cancel, flush or state.
- Hooks hold no reactive state unless you opt in with a `selector` argument or
  the instance's `Subscribe` component. Select only what the UI shows, such as
  `isPending`.
- `useDebouncer` cancels pending work on unmount. Pass `onUnmount: (d) =>
  d.flush()` when the last call must still run.
- For debounced search, debounce the value that enters the query key, not the
  `queryFn`, so Query keeps caching and cancellation (design advice, not from
  the docs).
