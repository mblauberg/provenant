# TanStack Table v9 reference

Read when writing or reviewing `useTable` code. Checked against
`@tanstack/react-table` 9.3.1.

## Setup

```tsx
const features = tableFeatures({
  rowSortingFeature,
  rowSelectionFeature,
  columnVisibilityFeature,
  columnPinningFeature,
  sortedRowModel: createSortedRowModel(), // client-side sorting only
});
const columnHelper = createColumnHelper<typeof features, Row>();
const columns = columnHelper.columns([columnHelper.accessor("name", { header: "Name" })]);
const table = useTable({ features, columns, data, getRowId: (r) => r.id });
```

- A row model sits in `tableFeatures()` after the feature it needs.
  `getCoreRowModel()` is automatic.
- `stockFeatures` registers everything, as v8 did. Use it only mid-migration.
- Keep `columns`, `features` and `data` reference-stable between renders.
- Renamed from v8: `useReactTable` to `useTable`, `getState()` to
  `table.state`, `sortingFn` to `sortFn`, pinning `left`/`right` to
  `start`/`end` (`column.pin('start')`, no aliases), `columnSizingInfo` to
  `columnResizing`.

## State

- Default: the table owns state; set `initialState` only.
- Controlled slice: `state: { sorting }` with `onSortingChange`.
  `onStateChange` is gone; use `table.store.subscribe` for all changes.
- External atoms: `useCreateAtom` from TanStack Store, passed as
  `atoms: { sorting }`. Use this to share sort and filter state with the
  query key. `table.reset()` resets internal atoms only; use feature resets
  such as `table.resetPagination()` for external ones.
- The second argument to `useTable` is a selector for `table.state`. Use
  `table.Subscribe` with `selector` or `source` to re-render only where needed.

## Server-side tables

1. Sort, filter and cursor state live in an atom or router search params.
2. That state is in the `queryOptions` key; the `queryFn` sends it to the API.
3. Set `manualSorting`, `manualFiltering` and `manualPagination` to `true`.
   Do not register `sortedRowModel` or `filteredRowModel`: the server owns
   order.
4. Show previous rows while the key changes (`keepPreviousData`).
5. Keyset cursors have no page index: store the cursor chain, and reset it
   when sort or filter changes.
6. Selection spans pages: key it by `getRowId`, store ids not rows, and say
   what "select all" means (this page or all matches).

## Column pinning and preferences

Pinning, visibility, order and sizing are plain serialisable state. Save them
through the app's preference API, seed with `initialState` or atoms, and drop
stored ids that are no longer column ids. Persist sort or selection only if
the product asks.

## Virtualisation (`@tanstack/react-virtual` 3)

- Table has no virtualiser. Render `getVirtualItems()` over
  `table.getRowModel().rows`, with `count: rows.length`, `getScrollElement`,
  `estimateSize` and `overscan`.
- Native table layout fights positioned rows. The official guide sets
  `table` and `thead` to `display: grid`, `tr` to `display: flex`, a sticky
  `thead`, and a relative `tbody` sized by `getTotalSize()` with rows moved by
  `translateY(virtualRow.start)`. Size cells from `column.getSize()`.
- For variable heights set `data-index` and `ref={node => virtualizer.measureElement(node)}`.
  For fixed heights skip measuring.
- Call `scrollToIndex(0)` when a new sort replaces the dataset.
- Unrendered rows are not in the DOM, so set `aria-rowcount` and
  `aria-rowindex`, and keep the focused row rendered when arrow keys move
  focus. Scroll with `scrollToIndex` before focusing a row that is off screen.
- Infinite scroll: flatten pages into `data`, fetch the next page near the
  end, and keep `count` equal to the loaded rows.

## React Compiler

- v9 is compatible; the v8 `'use no memo'` workaround is not needed.
- A compiled child that gets only `row`, `cell`, `column` or `header` can skip
  re-rendering and show stale selection or sort. Put `table.Subscribe` or
  `Subscribe` inside the child that reads the state.
