---
name: tanstack
description: "Use for TanStack Query v5, Router (file routes), Table v9, Virtual, Form, Store, Pacer, Hotkeys, Charts, Devtools and DB: keys, queryOptions, loaders, useTable, useForm, selectors. Not for ordinary component state (TanStack Store work stays here), render speed (react-performance) or backend caches."
---

# TanStack ecosystem: judgement layer

This skill holds cross-library rules and pitfalls. API detail comes from the
installed versions, so confirm them first. Project conventions win over this file.

## Version-matched API first

Read the lockfile and installed declarations before using an API. Consult the
[dated version notes](references/releases-2026.md) and
[version-matched documentation](references/version-matched.md). Flag unrelated
migration sites for implement or refactor.

## Where agents slip

- **Incomplete keys:** the key holds every value the `queryFn` reads.
- **Duplicated query contracts:** one `queryOptions` factory per resource,
  reused by hook, loader, prefetch and cache reads.
- **Failed requests that resolve:** `fetch` resolves on 4xx and 5xx. The
  `queryFn` throws for them and forwards `signal`.
- **Status:** `isPending` means no data and no error yet (a failed first fetch is
  not pending); `isFetching` is a running request. List
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
- Tables and virtual rows: [table](references/table.md). Never ship
  `stockFeatures`. Server tables set the `manual*` options and hold state in
  the query key. Virtualise only hundreds of rendered rows.
- Debounce, throttle, queue: [pacer](references/pacer.md) (beta).
- File routes, router-plugin, Hotkeys and Devtools: load the Intent skills
  shipped with the installed packages (`router-core` children, `router-plugin`,
  `hotkeys-core`, `devtools-production`). Keep devtools dev-only and
  tree-shaken from production; check the bundle only if that lane may run the
  project's own build. Devtools packages need compatible versions, per their
  peer ranges.
- Collections and live queries: [db](references/db.md) (experimental).
  Charts: use version-matched docs; no reference here.

## Verification

Support cache freshness and invalidation claims with a request trace or a focused component test. Record the evidence path.


Test public hooks/components with a fresh `QueryClient` and `retry: false`.
Suggest missing Query ESLint rules; never add them unasked. See
[verification](references/verification.md).
