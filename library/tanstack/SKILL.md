---
name: tanstack
description: "Use for TanStack Query v5, Router (file routes), Table v9, Virtual, Form, Store, Pacer, Hotkeys, Charts, Devtools and DB: keys, queryOptions, loaders, useTable, useForm, selectors. Not for local UI state, render speed (react-performance) or backend caches."
---

# TanStack ecosystem: judgement layer

This skill holds cross-library rules and pitfalls. API detail comes from the
installed versions, so confirm them first. Project conventions win over this file.

## Version-matched API first

1. Read the installed versions from the lockfile or `node_modules`.
2. Load matching skills read-only from the project root, which must have
   `node_modules` installed: `DO_NOT_TRACK=1 TANSTACK_CLI_TELEMETRY_DISABLED=1
   npx -y @tanstack/intent@0.5.6 list`, then `... load <package>#<skill>`.
   Intent reads local packages, so its skills match what is installed.
3. For gaps, use the read-only CLI (network needed, docs follow the major
   version, not the patch): `npx -y @tanstack/cli@0.72.0 search-docs "<q>"
   --library <id> --framework react`, and `... doc <id> <path>` with the same
   env vars. Use `libraries` to list ids.
4. Never run `intent install`, `intent hooks`, `intent setup`, or CLI `create`,
   `add`, `pin-versions`, `clean-demos` or `telemetry`. They write files.

Details and limits: [version-matched](references/version-matched.md).

## API newer than most training data

- **Imperative calls:** Query 5.10x deprecates fetch/prefetch/ensure methods.
  Read [migration mappings](references/imperative-queries.md) first. Flag
  existing sites rather than migrating them inside unrelated changes.
- **`staleTime: 'static'` overrides invalidation** for active observers. Keep
  it to one-off imperative calls, never a hook a mutation must refresh.
- **Table v9 is not v8:** read [references/table.md](references/table.md).
- **Store and Form:** `useStore` is deprecated for `useSelector`. See
  [store](references/store.md) and [form](references/form.md).

## Where agents slip

- **Incomplete keys:** the key holds every value the `queryFn` reads.
- **Duplicated query contracts:** one `queryOptions` factory per resource,
  reused by hook, loader, prefetch and cache reads.
- **Failed requests that resolve:** `fetch` resolves on 4xx and 5xx. The
  `queryFn` throws for them and forwards `signal`.
- **Status:** `isPending` is no data; `isFetching` is a running request. List
  key changes use `placeholderData: keepPreviousData`.
- **Freshness:** choose `staleTime` from the data; never disable focus
  refetching to hide a bad value.
- **Tracked results:** destructure only fields read; no rest-spread.
- **Optimistic updates:** show `variables` before editing the cache; money is
  never optimistic. See [references/mutations.md](references/mutations.md).
- **Query data is not component state:** derive or use `select`, never copy to
  `useState`.
- **`skipToken`:** `refetch()` cannot run that query.
- **`useSuspenseQuery`:** no `enabled` or `placeholderData`.

## Routing by library

- Router loaders with Query: [router integration](references/router.md).
  File routes, the bundler plugin and generated tree:
  [file routes](references/file-routes.md).
- Tables and virtual rows: [table](references/table.md). Never ship
  `stockFeatures`. Server tables set the `manual*` options and hold state in
  the query key. Virtualise only hundreds of rendered rows.
- Debounce, throttle, queue: [pacer](references/pacer.md) (beta).
- Shortcuts: [hotkeys](references/hotkeys.md). Devtools:
  [devtools](references/devtools.md). Collections and live queries:
  [db](references/db.md) (experimental). Charts: use version-matched docs;
  no reference here.

## Verification

Test public hooks/components with a fresh `QueryClient` and `retry: false`.
Suggest missing Query ESLint rules; never add them unasked. See
[verification](references/verification.md).
