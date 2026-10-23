# Console and menu bar: visual and interaction design

Status: **proposed with the spec**, 2026-10-22. Companion to
[the observability console spec](observability-console.md), which owns
requirements, data and delivery; this file owns visual and interaction design
for the console window and the menu bar panel of the Provenant app
(`apps/provenant`, renamed from `apps/quota-menubar`). Host
names in wireframes (workshop, studio) are examples.

Basis. Every API named below was checked against the macOS 27.0 SDK
interfaces with Swift 6.4: the `SwiftUI`, `SwiftUICore`, `Charts` and AppKit
interfaces were searched for each symbol and its `@available` line recorded.
Colour values were resolved from `NSColor` on macOS 27. Palette checks used
the dataviz validator (OKLab, colour-vision simulation, WCAG contrast). Apple
guidance comes from the Human Interface Guidelines (HIG) as published on
developer.apple.com and from the WWDC25 and WWDC26 sessions listed under
Sources.

## 1. Design principles

1. **Content first, glass only for navigation and controls.** Liquid Glass
   "forms a distinct functional layer for controls and navigation elements"
   and you should not "use Liquid Glass in the content layer"
   ([HIG Materials](https://developer.apple.com/design/human-interface-guidelines/materials)).
   Tables, charts, the context bar, diffs and cards are content. Apple's own
   example of what not to glass is a table view
   ([Meet Liquid Glass, WWDC25](https://developer.apple.com/videos/play/wwdc2025/219/)).
2. **Standard components before custom ones.** `NavigationSplitView`,
   toolbars, inspectors, search, sheets and menus pick up Liquid Glass, scroll
   edge effects and accessibility adaptations for free. Remove custom
   backgrounds from bars and split views
   ([Adopting Liquid Glass](https://developer.apple.com/documentation/technologyoverviews/adopting-liquid-glass)).
3. **Glass sparingly, never glass on glass.** "Always avoid glass on glass"
   (WWDC25 219). Group any custom glass in one `GlassEffectContainer`, which is
   also the performance path
   ([Applying Liquid Glass to custom views](https://developer.apple.com/documentation/swiftui/applying-liquid-glass-to-custom-views)).
4. **Colour carries meaning in the content layer, not in the chrome.** Tint
   glass only for a primary action; "if you want to imbue color into your
   app, do it in the content layer instead" (WWDC25 219). Toolbars stay
   monochrome
   ([HIG Color](https://developer.apple.com/design/human-interface-guidelines/color)).
5. **Never colour alone.** Every state has a symbol and words; every estimate
   has a visible mark
   ([HIG Accessibility](https://developer.apple.com/design/human-interface-guidelines/accessibility),
   [HIG Charts](https://developer.apple.com/design/human-interface-guidelines/charts)).
6. **Honest numbers.** Exact, apportioned, derived, estimated and unavailable
   are always distinguishable. Unknown is shown as unknown, never as zero.
7. **Glanceable, then drill down.** A summary line, one bar, a legend; detail
   only on disclosure or selection ("keep a chart simple, letting people choose
   when they want additional details",
   [HIG Charting data](https://developer.apple.com/design/human-interface-guidelines/charting-data)).
8. **Quiet by default.** Nothing animates, polls or wakes the CPU unless the
   user is looking or data changed
   ([Energy Efficiency Guide for Mac Apps: Timers](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/Timers.html)).
9. **Host-agnostic.** Zero, one or many machines look like the same app.
   "This Mac" is a host like any other; no host name appears in code or layout
   assumptions.

## 2. Layer model

### 2.1 What gets glass and what never does

| Surface | Layer | Treatment | How |
|---|---|---|---|
| Window toolbar and title area | Navigation | System glass, automatic | `.toolbar { }` with no custom background |
| Sidebar (source list) | Navigation | System floating glass sidebar, automatic | `NavigationSplitView` + `.listStyle(.sidebar)` |
| Inspector column | Navigation | System treatment, automatic | `.inspector(isPresented:content:)` |
| Search field | Navigation | System glass field, top trailing | `.searchable(...)` on the `NavigationSplitView` |
| Sheets, popovers, menus, alerts | Navigation | System glass, automatic | standard presentation APIs |
| Fleet graph floating controls (zoom out, zoom in, fit, legend) | Control over content | The only custom glass in the app | `GlassEffectContainer` + `.buttonStyle(.glass)` |
| Menu bar panel window | Navigation | System panel material, automatic | `MenuBarExtra` `.window` style; no glass added inside |
| Tables, lists of sessions, lanes, telemetry rows | Content | Never glass | `Table`, `List` |
| Context distribution bar, legend, charts | Content | Never glass | `Canvas`, Swift Charts |
| State text and diffs | Content | Never glass | `Text` in `ScrollView`, monospaced |
| Account cards, telemetry tiles | Content | Never glass; standard grouped background | `GroupBox` |
| Fleet graph nodes | Content | Never glass; card on window background | plain views with separator stroke |

The Fleet graph is the one place where controls float over a scrolling,
zoomable canvas, which is exactly the case Apple designed custom glass for
(the Maps and Freeform pattern). Everywhere else the system already supplies
the glass. That keeps the on-screen glass count at the system's own surfaces
plus at most one container.

### 2.2 APIs and availability

All verified in the macOS 27.0 SDK interfaces.

| API | Exact signature (abridged) | macOS |
|---|---|---|
| `glassEffect` | `func glassEffect(_ glass: Glass = .regular, in shape: some Shape = DefaultGlassEffectShape()) -> some View` | 26.0 |
| `Glass` | `.regular`, `.clear`, `.identity`, `.tint(_ color: Color?)`, `.interactive(_ isEnabled: Bool = true)` | 26.0 |
| `GlassEffectContainer` | `init(spacing: CGFloat? = nil, content:)` | 26.0 |
| `glassEffectID` | `func glassEffectID(_ id: (some Hashable & Sendable)?, in namespace: Namespace.ID)` | 26.0 |
| `glassEffectUnion` | `func glassEffectUnion(id: (some Hashable & Sendable)?, namespace: Namespace.ID)` | 26.0 |
| `glassEffectTransition` | `.matchedGeometry`, `.materialize`, `.identity` | 26.0 |
| Glass button styles | `.buttonStyle(.glass)`, `.buttonStyle(.glassProminent)` | 26.0 |
| `ToolbarSpacer` | `init(_ sizing: SpacerSizing = .flexible, placement: ToolbarItemPlacement = .automatic)`; `SpacerSizing.fixed`, `.flexible` | 26.0 |
| `sharedBackgroundVisibility` | `func sharedBackgroundVisibility(_ visibility: Visibility) -> some ToolbarContent` | 26.0 |
| `visibilityPriority` | `func visibilityPriority(_ priority: ToolbarItemVisibilityPriority)` on toolbar content | 26.1 |
| `scrollEdgeEffectStyle` | `func scrollEdgeEffectStyle(_ style: ScrollEdgeEffectStyle?, for edges: Edge.Set)`; `.automatic`, `.hard`, `.soft` | 26.0 |
| `scrollEdgeEffectHidden` | `func scrollEdgeEffectHidden(_ hidden: Bool = true, for edges: Edge.Set = .all)` | 26.0 |
| `safeAreaBar` | `func safeAreaBar(edge: VerticalEdge, alignment: HorizontalAlignment = .center, spacing: CGFloat? = nil, content:)` | 26.0 |
| `backgroundExtensionEffect` | `func backgroundExtensionEffect()` and `(isEnabled: Bool)` | 26.0 |
| `searchToolbarBehavior` | `func searchToolbarBehavior(_ behavior: SearchToolbarBehavior)` | 26.0 |
| `ConcentricRectangle` | `struct ConcentricRectangle: Shape` | 26.0 |
| `labelIconToTitleSpacing` | `func labelIconToTitleSpacing(_ value: CGFloat)` | 26.0 |
| `.tabs` picker style | `PickerStyle.tabs` (`TabsPickerStyle`): "a segmented picker with a visual treatment that distinguishes tab navigation from value selection"; VoiceOver announces options as tabs | **27.0** |
| `NSGlassEffectView`, `NSGlassEffectContainerView` | AppKit equivalents | 26.0 |
| `NSGlassEffectView.effectIsInteractive` | pointer-tuned interactive glass | **27.0** |
| `inspector(isPresented:content:)`, `inspectorColumnWidth(min:ideal:max:)`, `InspectorCommands` (Control-Command-I) | | 14.0 |
| `DisclosureTableRow`, `TableColumnCustomization`, `alternatingRowBackgrounds` | | 14.0 |
| `defaultLaunchBehavior(_:)`, `restorationBehavior(_:)` | scene modifiers | 15.0 |
| Charts vectorised `LinePlot`, `AreaPlot`, `BarPlot` | | 15.0 |
| `EnvironmentValues.appearsActive` | dim custom content in inactive windows (WWDC26 269 advice) | available |
| `accessibilityReduceTransparency`, `colorSchemeContrast`, `accessibilityReduceMotion`, `accessibilityDifferentiateWithoutColor` | environment values | available |

**Not available on macOS** (present only for iOS or visionOS in this SDK), so
the design does not use them: `tabViewBottomAccessory`, `ToolbarOverflowMenu`,
`ToolbarItemPlacement.topBarPinnedTrailing`, `listSectionMargins`. No
`toolbarMinimizeBehavior` symbol exists in the macOS interface.

### 2.3 Accessibility fallbacks

Apple states that Reduce Transparency "makes Liquid Glass frostier", Increase
Contrast "makes elements predominantly black or white and highlights them with
a contrasting border", and Reduce Motion "disables any elastic properties",
and that these "are available automatically whenever you use the new
material" (WWDC25 219). So system surfaces and the one custom
`glassEffect` container need no manual fallback. The rules for the rest:

- **Reduce Transparency.** Any standard `Material` used (only the sticky
  diff gutter header, if at all) switches to an opaque
  `Color(nsColor: .windowBackgroundColor)` when
  `accessibilityReduceTransparency` is true. Never fake glass with blur and
  opacity in the content layer.
- **Increase Contrast.** The categorical palette switches to its increased
  contrast steps (section 5.2). The context bar adds a 1 pt
  `Color(nsColor: .separatorColor)` outline round each segment in addition to
  the 2 pt gap. Diff tints rise from 12% to 20% opacity and gain a 2 pt
  leading rule. Cards keep the system `GroupBox` border.
- **Reduce Motion.** The console's own `withAnimation` calls (disclosure, bar
  re-segmentation, ring changes) become opacity cross-fades or no animation.
  Glass morphing in the Fleet control cluster uses
  `glassEffectTransition(.identity)` when Reduce Motion is on.
- **Differentiate Without Colour.** Bar segments wider than 18 pt draw their
  category symbol inside; status always pairs symbol and text.
- **Inactive window.** Following WWDC26, custom sidebar content (the host
  rows' freshness dots) dims with `appearsActive == false`.

## 3. Window anatomy

### 3.1 Models from Apple's own apps

System Settings is the model for Settings (sidebar, grouped forms, no
inspector); Finder for the sidebar sections and Hosts; Activity Monitor for
Telemetry and the Fleet List mode; Xcode for the Fleet inspector; Music for
the Sessions three-column split.

### 3.2 Scene structure

Two processes in one bundle (spec, Processes and lifecycle). The menu bar
agent has no console code:

```swift
@main struct ProvenantMenuApp: App {                               // LSUIElement agent, always on
    var body: some Scene {
        MenuBarExtra { Panel() } label: { MenuBarLabel() }
            .menuBarExtraStyle(.window)
    }
}
```

The console is a separate process, started on demand by the `provenant` URL
scheme, the Dock, Spotlight or `provenant console`:

```swift
@main struct ProvenantConsoleApp: App {
    var body: some Scene {
        Window("Provenant", id: "console") { Console() }          // one console, not a WindowGroup
            .defaultSize(width: 1120, height: 720)
            .windowResizability(.contentMinSize)                   // minimum 760 x 480 from content
            .commands { SidebarCommands(); InspectorCommands(); ConsoleCommands() }
    }
}
```

- The console is a regular app (Dock icon, app switcher) while it runs and
  quits when its last window closes, which frees all of its memory. The agent
  stays an accessory with no Dock icon. The two share state only through files
  and the CLI. The console works the same with the menu bar off.
- Modules the user has switched off are not constructed: their sidebar items,
  toolbar items and sections are absent, not greyed out.
- Column widths: sidebar `navigationSplitViewColumnWidth(min: 180, ideal: 200,
  max: 260)`; Sessions list `min: 280, ideal: 340, max: 440`; inspector
  `inspectorColumnWidth(min: 260, ideal: 300, max: 400)`.
- `.searchable(text:)` goes on the `NavigationSplitView` so one top-trailing
  field searches the current section (WWDC25 323).
- Scroll edge effect: leave `.automatic`, which is the hard style on macOS and
  suits pinned table headers (WWDC25 356). One effect per pane, never stacked.
- `backgroundExtensionEffect` is not used: it exists for hero imagery under a
  sidebar, and the console has none.

### 3.3 Sidebar

```text
+----------------------+
| Activity             |   section headers in title case (macOS 26 lists
|   Fleet          (7) |   no longer force capitals)
|   Sessions           |
| Insight              |
|   Telemetry          |
|   Accounts       (!) |   badge only when an account is exhausted or signed out
| Hosts                |
|   This Mac     · now |   one row per host; symbol from host kind
|   workshop    · 40 s |   dot plus age text; offline shows wifi.slash
|   studio   offline 2h|   and the word "offline"
|                      |
|   Settings           |   last row, separated by space, gearshape
+----------------------+
```

| Item | Symbol | Shortcut |
|---|---|---|
| Fleet | `point.3.connected.trianglepath.dotted` | Command-1 |
| Sessions | `text.bubble` | Command-2 |
| Telemetry | `chart.bar.xaxis` | Command-3 |
| Accounts | `gauge.with.dots.needle.50percent` | Command-4 |
| Settings | `gearshape` | Command-5 and Command-comma |
| Host rows | `laptopcomputer`, `macmini` or `desktopcomputer` (user-chosen per host, default `desktopcomputer`); offline `wifi.slash` | none |

- Host status is a **Hosts** section, not a strip at the bottom of the
  sidebar. The HIG says to "avoid putting critical information or actions at
  the bottom of a sidebar" because windows get moved so the bottom edge is
  hidden
  ([HIG Sidebars](https://developer.apple.com/design/human-interface-guidelines/sidebars)).
  Selecting a host opens a small Host page (status, last pull, projector
  version, error, Pull Now). With no peers the section shows one row, "This
  Mac". This is Finder's Locations pattern.
- Sidebar icons follow the user's accent colour. The only fixed colour is the
  Accounts badge, which carries meaning (the HIG allows fixed colours "if you
  use them sparingly" to clarify meaning).
- Fleet appears with Slice 2, reading `provenant lanes --json`; the Hosts
  section appears with federation (spec, Delivery plan). Until then Sessions,
  Fleet and Accounts are the populated sections.

### 3.4 Toolbar

Rules: at most three groups (HIG Toolbars), icons not text inside a group,
an accessibility label on every icon, every toolbar action also in the menu
bar, titles under 15 characters, never the app name as the title.

```text
[sidebar] Title                [Scope v][Project v]  [Graph|List]  ::  [Pull] [Inspector]   (search)
          subtitle             '---- group 1 ----'   '- group 2 -'  fixed  '--- group 3 ---'
```

- **Leading:** system sidebar toggle, `navigationTitle` plus
  `navigationSubtitle` (for example "Fleet" and "3 hosts, 7 running").
- **Group 1, filters:** scope menu (All Hosts, This Mac, Mine) and project
  menu. `visibilityPriority(.high)` so they survive narrow windows.
- **Group 2, view mode:** Graph or List (Fleet), period (Telemetry).
- `ToolbarSpacer(.fixed)`.
- **Group 3, actions:** Pull Now or Refresh (`arrow.clockwise`, Command-R) and
  Inspector toggle (`sidebar.right`, Control-Command-I from
  `InspectorCommands`).
- **Freshness text** ("Updated 40 s ago") sits beside group 3 with
  `.sharedBackgroundVisibility(.hidden)` so it does not look like a button.
- **Search:** system field, top trailing, Command-F.

### 3.5 Fleet

```text
+--------------------------------------------------------------------------------------+
| [=] Fleet            [All Hosts v][Project v]  [Graph|List]   Updated 40 s  [R] [|]  (Q)|
|     3 hosts, 7 running                                                               |
+-----------------+--------------------------------------------------+-----------------+
| Activity        |  This Mac                 workshop               | Lane 4f2c       |
| > Fleet      7  |  +----------------+       +----------------+     |                 |
|   Sessions      |  | (C) chair      |------>| (X) senior     |     | Codex, senior   |
| Insight         |  |  this session  |       |  running 12 m  |     | Running 12 m    |
|   Telemetry     |  +-------+--------+       +----------------+     |                 |
|   Accounts      |          |                                       | Route           |
| Hosts           |   +------v-------+   +--------------+            |  requested  ... |
|   This Mac  now |   | (O) junior   |   | (C) review   |            |  resolved   ... |
|   workshop 40 s |   |  done  4 m   |   |  in review   |            |  observed   ... |
|                 |   +--------------+   +--------------+            | Host  workshop  |
|   Settings      |                                                  | Tokens ≈41k     |
|                 |                         +-------------------+    | Cost  $0.30     |
|                 |                         | ( - ) ( + ) (fit) |    | result.md       |
|                 |                         +-------------------+    |  first lines... |
|                 |                    glass control cluster,        | [Reveal][Copy]  |
|                 |                    bottom trailing               | [Cancel...]     |
+-----------------+--------------------------------------------------+-----------------+
```

- **Graph mode:** host columns, seats as roots, lanes beneath. Edges in one
  `Canvas` behind the nodes; nodes are real views (selectable, focusable,
  VoiceOver elements) placed by a custom `Layout`. Nodes are content: window
  background, 1 pt separator stroke, 10 pt corner radius, provider symbol,
  route, state symbol and words, elapsed time.
- **Floating controls:** one `GlassEffectContainer(spacing: 8)` with three
  `.buttonStyle(.glass)` buttons (zoom out, zoom in, fit). This is the only
  custom glass in the app.
- **List mode:** `Table` with `DisclosureTableRow` (host, then seat, then
  lanes), columns State, Route, Host, Elapsed, Tokens, Cost. This is the
  accessible equivalent the spec asks for.
- **Inspector:** identity, three-form route, worktree and branch, timings,
  tokens, cost, review state, first lines of `result.md` on demand, actions
  Reveal in Finder (local only), Copy ID, Cancel (confirmation dialog anchored
  to the button).

### 3.6 Sessions (three columns)

```text
+--------------------------------------------------------------------------------------+
| [=] Sessions                         [All Hosts v][Project v]   Updated now  [R] (Q) |
+-----------------+--------------------------+-----------------------------------------+
| Activity        | This Mac                 |  provenant · docs/console   (C) Claude  |
|   Fleet         |  provenant               |  Opus, high · 3 compactions · [S] state |
| > Sessions      |  (C) docs/console        | [Overview | Context | State | Usage]    |
| Insight         |      57% ▬▬▬▬▬░░  [S] 3c |  '-- .tabs picker in a safeAreaBar --'  |
|   Telemetry     |  (X) feat/projector      |                                         |
|   Accounts      |      31% ▬▬▬░░░░      0c |   (pane content, section 4.1 and 4.6)   |
| Hosts           |  other-project           |                                         |
|   This Mac  now |  (C) main                |                                         |
|   workshop 40 s |      12% ▬░░░░░░  [S] 0c |                                         |
|                 | workshop · 40 s          |                                         |
|   Settings      |  provenant               |                                         |
|                 |  (X) fix/peer  44% ...   |                                         |
+-----------------+--------------------------+-----------------------------------------+
```

- **List:** `List` with sections per host, then project. Row: provider
  symbol, branch, last activity, a 4 pt mini context bar (the same `Canvas`
  component as the detail, section 4.1), percent used with
  `.monospacedDigit()`, compaction count ("3c" with an accessibility label
  "3 compactions"), and the badge `doc.badge.clock` (`[S]`) when state was
  re-injected at the last compaction.
- **Detail header:** project, branch, provider, model and effort, compaction
  count, state badge. Below it the section switcher as
  `Picker(...).pickerStyle(.tabs)` (macOS 27), placed in
  `safeAreaBar(edge: .top)` so it stays pinned and gets the scroll edge
  effect. On macOS 26 fall back to `.segmented`.
- **Tabs:** Overview, Context, State, Usage (spec). Command-Option-1 to 4
  switch tabs.
- **Peer sessions:** a session on another host lists with its summary row,
  but its Context and State tabs are local-only. They show "Content stays on
  workshop" unless `hosts.peers.<name>.allow_content_reads` is on, in which
  case a "Load from workshop" button fetches it once, without caching it on
  disk.

### 3.7 Context tab

Two figures of the same session, as they appear with each source. With an
exact count from Claude Code's `/context`:

```text
 Context                              As of turn 212 · now     [Count exactly]
 57% used                                                 142.0k / 250k
 [■■■■|■■■|■|■|■■|■■|■■■■■■■■■■■■■■■■■■■■■■|▧▧▧▧▧▧▧▧▧|                    ]
 Autocompacts at 205.0k, about 63.0k from now

 ■ (text.page)            System prompt                14.2k    5.7%   >
 ■ (wrench)               System tools                 12.9k    5.2%   >
 ■ (puzzlepiece)          MCP tools                     4.8k    1.9%   >  9 of 57 loaded
 ■ (list.bullet.rect)     MCP server instructions       1.1k    0.4%   >
 ■ (person.2)             Custom agents                 0.6k    0.2%   >  1
 ■ (brain)                Memory files                  6.1k    2.4%   >  7 files
 ■ (wand.and.stars)       Skills                        6.5k    2.6%   >  41
 ■ (bubbles)              Messages                     96.0k   38.4%   >
 ▧ (compress)             Autocompact buffer           45.0k   18.0%
 □ (square.dashed)        Free space                   63.0k   25.2%
   (circle.dashed)        MCP tools (deferred)         38.2k       -   >  48
   (circle.dashed)        System tools (deferred)       9.4k       -   >  12

 Categories counted by Claude Code /context at 14:02. Messages and free
 space are derived from the exact total. Per-tool MCP figures are
 apportioned; per-skill figures are estimated.
```

With the transcript only (no count yet, or counting off for this account):

```text
 ■ (text.page)            System prompt              ≈ 14.0k    5.6%   >
 ...
 ■ (bubbles)              Messages                   ≈ 89.0k   35.6%   >
 ▨ (questionmark.dashed)  Unattributed                  7.0k    2.8%

 ≈ Estimated from transcript content (chars/4). The total, free space and
   buffer are exact. Unattributed is the exact total minus the estimates.
```

The lower half of the tab:

```text
 Context over time                                     [Total | By category]
 250k ┤- - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - window
 205k ┤· · · · · · · · · · · · · · · · · · · · · · · · · · · · · · ·  autocompact
      │        /|          /|            /|
      │      /  |        /  |  (doc)   /  |      /
      │    /    |      /    |   ^    /    |    /
      │  /      |    /      |   |  /      |  /
    0 ┼--------C1---------C2---+-------C3------------------------ turns
                               state re-injected at C2 (5.4 KB)

 Compactions
  C3  turn 188  16:40   201.4k -> 38.9k  (−162.5k)  summary 14.1 KB  (doc) state re-injected 5.4 KB  >
  C2  turn 148  14:02   198.0k -> 41.2k  (−156.8k)  summary 13.0 KB  (doc) state re-injected 5.4 KB  >
  C1  turn  91  10:14   204.7k -> 36.0k  (−168.7k)  summary 12.2 KB  (!) nothing re-injected         >
```

Behaviour:

- Selecting a point on the chart (`chartXSelection(value:)`) sets "As of
  turn N"; it snaps to the nearest recorded snapshot (the latest turn or a
  compaction boundary) and the header, bar and legend redraw for it. Other
  turns show only their total. Escape returns to the latest turn.
- **Count exactly** runs `provenant session context --exact` for this
  session. It appears only when `observe.context.exact_counts` allows it for
  the session's account, and its help tag says that counting sends the
  counted text (system prompt, tools, memory files, skills, agents) to the
  provider's token-count endpoint through the provider's own client. For a
  finished or interactive session the result is a reconstruction (section
  4.3, probe state).
- Every category row with items is a disclosure (section 4.4). The count in
  the trailing column is exact.
- A compaction row's chevron opens the State tab at that compaction.

### 3.8 State tab

The State tab answers one question: at each compaction, what did the session
get back? The ground truth is the transcript. For Claude Code the
`SessionStart:compact` hook's output is recorded verbatim as a `hook_success`
attachment, whose header names the state file it came from; the hook caps it
at 8000 characters. No snapshot of the file is stored.

```text
 State at C2 · turn 148 · 14:02
 (doc) Re-injected at C2 · 5.4 KB · from .agent-run/sessions/7c1e/STATE.md
 [Injected | Diff | Summary | Current file]   Compare [Injected at C1 v] with [Injected at C2 v]

 +-------------------------------------------+----------------------------------------+
 |  12  12    ## Active lanes                 | Compaction summary C2 · 13.0 KB        |
 |  13      − - 4f2c codex senior: running    | (read from transcript, not stored)     |
 |      13  + - 4f2c codex senior: in review  |                                        |
 |      14  + - 91ab claude review: running   | The user asked for the console design  |
 |  14  15    ## Next actions                 | ...                                    |
 |  15  16    - Context bar uses Canvas       |                                        |
 +-------------------------------------------+----------------------------------------+
  2 added, 1 removed, 40 unchanged                 [Show unchanged lines]   [Copy diff]

 Re-attached after compaction
  (doc.text)  skills/session/references/chair-state.md        file
  (wand)      session                                         skill
  (checklist) 3 tasks                                          task status
```

- **Modes,** via a second `.tabs` picker:
  - *Injected:* the re-injected text at the selected compaction, read-only
    in SF Mono. This is the primary view.
  - *Diff:* unified diff between two re-injected payloads (default: the
    previous compaction against the selected one). Both texts come from the
    transcript.
  - *Summary:* the compaction summary beside the re-injected text, so the
    user can check whether the state recorded what the summary lost. Diff
    between successive summaries is available from the same picker.
  - *Current file:* the state file on disk now, secondary, with a banner when
    it differs from the last injected payload ("Changed since C3").
- **Pickers** list compactions by ordinal ("Injected at C1", "Injected at
  C2"); there are no time-based snapshots.
- **Summary source by provider:** exact for Claude Code and OpenCode;
  "Encrypted by provider" for Codex, where the re-injected developer and user
  messages from `replacement_history` show under Injected instead; "Not
  available" for agy and Cursor.
- **Warning:** when a compaction has no recorded `hook_success`, the banner
  shows `exclamationmark.triangle` and "Nothing was re-injected at C1".
- **Empty state:** "Nothing was re-injected at this compaction", with a note
  when the provider has no `SessionStart:compact` hook configured (Codex has
  none today), plus the compaction summaries alone.
- **Re-attached after compaction** lists the `file`,
  `compact_file_reference`, `invoked_skills` and `task_status` attachments
  between the boundary and the first assistant turn.
- **Peer sessions:** local-only, as in section 3.6.

### 3.9 Telemetry

```text
+------------------------------------------------------------------------------------+
| [=] Telemetry           [Group by: Model v]  [Last 7 days v]   Updated 2 m  [R] (Q) |
+-----------------+------------------------------------------------------------------+
| ...sidebar...   |  +--------------+ +--------------+ +--------------+ +-----------+ |
|                 |  | Cost per     | | Accepted     | | Unattributed | | Cache hit | |
|                 |  | accepted chg | | changes      | | spend        | | rate      | |
|                 |  | $1.84        | | 23           | | 4.1%         | | 87%       | |
|                 |  | down 12%     | | up 5         | | (!) up 1.2pt | | flat      | |
|                 |  +--------------+ +--------------+ +--------------+ +-----------+ |
|                 |  Cost by model                                                   |
|                 |  [ horizontal BarMark per group, one hue, value labels ]         |
|                 |  -----------------------------------------------------------------|
|                 |  Table: Group | Runs | Tokens | Cache read | Cost | Cost/accepted |
|                 |  -----------------------------------------------------------------|
|                 |  Findings (3): requested vs observed model mismatches         >  |
+-----------------+------------------------------------------------------------------+
```

- Tiles are `GroupBox` cards in a `Grid`, value in `.title` with
  `.monospacedDigit()`, change line with a direction symbol
  (`arrow.down.right`, `arrow.up.right`) and words, never colour alone.
- The chart is one hue (accent) because the job is magnitude, not identity
  (dataviz rule: colour by job). Grouping is a toolbar picker, not a legend.
- `Table` with `TableColumnCustomization` so the user can hide and reorder
  columns, persisted per pane.

### 3.10 Accounts

```text
+------------------------------------------------------------------------------------+
| [=] Accounts             [All Hosts v]                          Updated 1 m  [R]    |
+-----------------+------------------------------------------------------------------+
| ...sidebar...   |  +-----------------------------+  +-----------------------------+ |
|                 |  | Claude · Max                |  | Codex · Pro                 | |
|                 |  | [This Mac] [workshop]       |  | [This Mac]                  | |
|                 |  |   .--.                      |  |   .--.                      | |
|                 |  |  ( 62 )  Weekly  62% used   |  |  ( 18 )  Weekly  18% used   | |
|                 |  |   '--'   Resets Mon 09:00   |  |   '--'   Resets Thu 14:00   | |
|                 |  |          At this pace: 94%  |  |          On pace            | |
|                 |  |  5-hour  41%   resets 15:20 |  |  5-hour   7%   resets 16:05 | |
|                 |  |  > Pools and history        |  |  > Pools and history        | |
|                 |  +-----------------------------+  +-----------------------------+ |
|                 |  +-----------------------------+                                  |
|                 |  | Gemini · signed out         |                                  |
|                 |  | (person.slash) last seen    |                                  |
|                 |  |   Tue 18:20 · Weekly 40%    |                                  |
|                 |  | Sign in with the provider's |                                  |
|                 |  | CLI to resume.   [Forget...]|                                  |
|                 |  +-----------------------------+                                  |
+-----------------+------------------------------------------------------------------+
```

- Cards in a `LazyVGrid(columns: [GridItem(.adaptive(minimum: 300))])`,
  ordered by the report's headline choice, never re-sorted by the console.
- The ring shows the report's display window (weekly first). Shorter windows
  are rows beneath, not nested rings, so two percentages never compete in one
  glyph. Reset time is a fact; "At this pace" is labelled as an estimate on
  its own line.
- Disclosure reveals pools and their windows by label and a 30-day
  `quota_series` line chart.
- **Remembered accounts.** An account is added automatically the first time
  it signs in. When it signs out it stays, greyed, with its last cached
  reading and "Signed out · last seen <time>"; when it returns the card shows
  "Back since <time>" until the next reset. Forget (with confirmation)
  removes it; `quota.forget_after_days`, if set, forgets it automatically.
  This comes from the quota report; the console renders it.
- **Hosts.** Host badges come from peers' cached quota snapshots, merged only
  while a viewer is open and deduplicated by the account's fingerprint label.
  With zero peers there are no badges.
- Display strings come only from the report's presentation fields (spec,
  Accounts). This keeps the console aligned with the quota report display
  work.

### 3.11 Settings: pane, not a separate window

Settings is a sidebar pane laid out like System Settings: `Form` with
`.formStyle(.grouped)`, sections Modules, Hosts, Projector, Context, Console,
Telemetry and Profile, in the order of the spec's settings model, with a
Modules group first.
Command-comma and the app menu's Settings item select this pane (opening the
console if needed). There is no separate `Settings` scene, so there is one
place to look and one source of truth.

```text
 Settings
 +-- Modules ------------------------------------------------------------+
 | Menu bar item                                             [  on  ]    |
 | Fleet graph                                               [  on  ]    |
 | Exact context counts                                      [  on  ]    |
 | Cross-host federation                                     [  on  ]    |
 | Notifications                                             [ off  ]    |
 |   A switched-off module is not loaded and uses no memory or CPU.      |
 |   Changes apply the next time the console or panel opens.             |
 +-----------------------------------------------------------------------+
 +-- Hosts --------------------------------------------------------------+
 | This Mac's name                       mini                    [Edit]  |
 | workshop   ssh workshop.tailnet · online                       (i)    |
 | studio     ssh studio · offline 2 h                            (i)    |
 |                                                   [+ Add Host...]     |
 +-----------------------------------------------------------------------+
 +-- Projector (only once installed) ------------------------------------+
 | Projector enabled                                         [  on  ]    |
 | Pull interval while watched                      [ 60 ] seconds       |
 |   15 to 3600 seconds                                                  |
 +-----------------------------------------------------------------------+
 +-- Context ------------------------------------------------------------+
 | Exact counts, per account                          [ Off | On ]       |
 |   Counting sends the counted text to the provider's token-count       |
 |   endpoint through the provider's own client.                         |
 +-----------------------------------------------------------------------+
```

- Each field validates through `provenant settings validate` as it changes;
  the error appears under the field in `.secondary` with
  `exclamationmark.triangle.fill`, and the field keeps the user's text.
- Simple fields apply when committed (Return or focus leaves), System
  Settings style, through `settings set --expect-rev`.
- Adding or editing a host opens a sheet with Cancel and Save, because its
  fields must change together. Save writes them in one call to
  `provenant settings set hosts.peers.<name>.*`.
- A revision conflict shows an inline banner "Changed elsewhere. Reloaded."
  with the new value highlighted once.

## 4. Components

### 4.1 Context distribution bar

**Decision: a custom `Canvas` component, not Swift Charts `BarMark`.**

Reasons, in order of weight:

1. **It appears in every Sessions row.** A 4 pt mini bar per row across a few
   hundred rows must be cheap. A `Canvas` is one view with one draw pass and
   no per-segment view identity. A `Chart` per row builds scales, an axis
   layout pass and an accessibility tree per mark. The same component, at
   8 pt, is the detail bar.
2. **It is not a chart in the HIG sense.** It has no axis, grid or scale for
   the user to read; it is a part-to-whole meter whose values live in the
   legend beside it. Swift Charts' strengths (axes, selection, Audio Graphs)
   go unused.
3. **The visual needs are drawing primitives.** 2 pt gaps between segments,
   rounded outer ends only, hatched fills for unattributed and buffer
   (`GraphicsContext.Shading.tiledImage`, available), and an empty track for
   free space are each a few lines in a `Canvas`.
4. **Swift Charts stays for real charts.** The context-over-time chart,
   Telemetry and quota history use Swift Charts, where axes and selection
   matter. Charts lives in the dyld shared cache, so linking it costs
   little; the cost is per chart instance, which this keeps low.

Specification:

- Height 8 pt in the detail, 4 pt in list rows; corner radius half the
  height on the two outer ends only; 2 pt gap in the window background colour
  between segments; segments narrower than 1 pt are drawn at 1 pt and
  borrow from free space so tiny categories stay visible.
- Order is fixed: System prompt, System tools, MCP tools, MCP server
  instructions, Custom agents, Memory files, Skills, Messages, Unattributed
  (transcript estimates only), Autocompact buffer, then Free space as the
  empty track. Colour follows the category, never its rank.
- Unattributed: 45 degree hatch in `secondaryLabelColor` at 35%. Buffer: 135
  degree hatch in `tertiaryLabelColor`. Free: `quaternarySystemFill`.
- The bar is one accessibility element: "Context, 57 percent used, 142
  thousand of 250 thousand tokens, autocompacts at 205 thousand". The legend
  rows are the navigable detail.
- Animation: segment widths animate with `.snappy(duration: 0.25)` when the
  selected turn changes; none under Reduce Motion.

### 4.2 Legend rows

```text
 [swatch 10x10, r3] [symbol, secondary]  Name                    ≈ 14.2k    5.7%  [count] [>]
```

- Columns in a `Grid` so numbers align: tokens right-aligned, percent
  right-aligned, both `.monospacedDigit()`. Names `.body`, numbers `.body`,
  counts `.callout` in `.secondary`.
- Percent is the share of the **context window** (250k), as in Claude's own
  panel, so the column plus free space sums to 100%. One decimal; "<0.1%"
  below 0.05%.
- Swatch plus symbol plus name give three cues; text never takes the
  category colour (dataviz rule: text wears text tokens).
- Hover highlights the matching bar segment (other segments drop to 40%
  opacity) and vice versa; with Reduce Motion the change is instant.
- Optional trailing "Change since last compaction" column (the spec's delta)
  appears when the pane is wider than 640 pt, via `ViewThatFits`.

### 4.3 Exactness marks

| State | Number shown | Mark | Help tag and VoiceOver |
|---|---|---|---|
| Exact | `142.0k` | none | "exact, counted by the provider" |
| Apportioned | `≈ 0.9k` | `≈` prefix | "about 0.9 thousand, a share of an exact total split by length" |
| Estimated | `≈ 14.2k` | `≈` prefix, same weight and colour | "about 14.2 thousand, estimated with chars/4" (the estimator id from the record) |
| Derived (Messages, Free space) | `96.0k` | none | "exact total minus counted categories" |
| Unattributed (transcript estimates only) | `7.0k` | hatch | "exact total minus the raw estimates; estimates are never scaled to fit" |
| Unavailable | `-` | dash | "not recorded by this provider version" |
| Deferred (not in context) | `38.2k` and `-` for percent | greyed row | "deferred, not loaded into context" |

- One footnote under the legend states the method and which figures are
  exact, naming the source (`/context` count or transcript) and the estimator
  id from the record.
- **Reconstructed counts.** When the count came from a resume probe of a
  finished or interactive session (`probe_reconstructed`), the footnote reads
  "Counted by Claude Code /context at 14:02 for the configuration now; may
  differ from what the session carried", with `clock.arrow.circlepath`. A
  reconstruction never replaces a recorded figure for an earlier turn.
- The `≈` is a real character in the string, so it survives copy and is
  read by VoiceOver; it is never conveyed by colour or italics alone.
- Where Claude Code recorded `contentDiffersFromDisk`, the memory file row
  shows `exclamationmark.circle` and "Differs from file on disk".

### 4.4 Drill-down with per-item sizes

Each category row is itself a `DisclosureGroup`; there are no separate
"MCP tools 93" rows. Expanded content:

```text
 v ■ (puzzlepiece)  MCP tools                    4.8k   1.9%   9 of 57 loaded
      Sort [Largest | Name]                                   Show deferred [ ]
      (server.rack) fabric                     ≈ 2.9k   ▬▬▬▬▬▬▬▬▬▬▬▬░░░░   6 tools
         fabric_dispatch                       ≈ 0.9k   ▬▬▬▬░░░░░░░░░░░░
         fabric_status                         ≈ 0.4k   ▬▬░░░░░░░░░░░░░░
         ...
      (server.rack) aws-mcp                    ≈ 1.9k   ▬▬▬▬▬▬▬▬░░░░░░░░   3 tools
```

| Category | Items listed | Item fields |
|---|---|---|
| System prompt | harness prompt blocks | block name, ≈ tokens |
| System tools | each built-in tool, loaded then deferred | name, ≈ schema tokens, loaded or deferred |
| MCP tools | grouped by server, then tool | server, tool, tokens (apportioned or estimated), loaded or deferred |
| MCP server instructions | each server | server, tokens (apportioned or estimated) |
| Custom agents | each agent | name, source (user, project, plugin), tokens |
| Memory files | each instruction and memory file, in load order | home-relative path, kind (user, project, nested, rule, memory index), tokens, differs-from-disk badge |
| Skills | listing entries, then skills loaded by invocation | name, source, ≈ listing tokens, ≈ body tokens when loaded, turn loaded |
| Messages | Conversation, Tool results (by tool, then the ten largest results), Files read (path), Compaction summary | ≈ tokens, turn |

- Item figures carry their own exactness mark (section 4.3): memory files and
  agents are exact when counted, MCP items apportioned, skills estimated.
- Item bars are `Capsule` widths relative to the **category** total, drawn in
  the category hue at 60% so they read as "part of this colour". They are
  decoration; each row's accessibility label states the number.
- Lists longer than 20 items show the 20 largest and "Show all 93". All item
  lists are lazy (`LazyVStack` inside the legend's scroll view) and load
  their detail only when first expanded.
- Context menu on an item: Copy Name, Copy Path, Reveal in Finder (local
  files only).

### 4.5 Compaction timeline

- Swift Charts. Total per turn as `LineMark` (or vectorised `LinePlot` when
  there are more than 500 turns), stacked `AreaMark` by category family in the
  "By category" mode using the categorical palette, window size and
  autocompact threshold as dashed `RuleMark`s with trailing labels.
- Each compaction is a vertical `RuleMark` labelled `C1`, `C2` in
  `.caption` text. A compaction with re-injected state carries a `PointMark`
  with the `doc.badge.clock` symbol as its annotation; one with nothing
  re-injected carries `exclamationmark.triangle`.
- `chartXSelection(value:)` drives "As of turn N" for the whole Context tab.
- Accessibility: Swift Charts' default Audio Graph, with a chart title
  ("Context size by turn") and summary ("3 compactions, peak 204.7
  thousand").
- The Compactions list below the chart is the table equivalent the HIG asks
  for; each row states before, after, removed, summary size and re-injection
  status in words and symbols.

### 4.6 Diff view

- SF Mono via `.font(.system(.body, design: .monospaced))`, line numbers in
  `.tertiary`, a gutter glyph column (`+`, `−`, space) that carries the
  meaning; tints are secondary: added `Color.green.opacity(0.12)`, removed
  `Color.red.opacity(0.12)` (0.20 and a 2 pt leading rule under Increase
  Contrast).
- Unchanged runs longer than six lines collapse to "40 unchanged lines" with
  a disclosure, like Xcode's comparison view.
- Text is selectable; Copy Diff copies unified diff text.
- Diffs compute off the main thread with a line diff (Swift's
  `CollectionDifference`) over two transcript texts, cached per pair in
  memory only.

### 4.7 Account cards and pool rings

- One `Ring` component shared by the console and the menu bar panel (the
  existing `Ring` in `App.swift` is the starting point): track in
  `.quaternary`, progress stroke with round caps, width `max(size / 8, 2)`.
  Sizes: 56 pt console card, 40 pt panel headline, 22 pt panel row, 10 pt
  inline window row.
- Ring tint is status: on pace and reserve `green`, ahead of pace (`ahead_of_pace`) `orange`,
  exhausted `red`, drawing on credits `blue`, unknown or signed out
  `secondary`. Legacy `deficit` receipts retain their orange tint. Ahead of pace
  means allowance remains; show the allowance estimate, credit runway (or unknown)
  and banked resets together. Expired windows show stale, never allowance spent.
  Status text and symbol sit beside it, always. These four hues
  are **reserved for status** and never used as context categories.
- The percentage sits inside rings 40 pt and larger in `.semibold`
  `.monospacedDigit()`; smaller rings put it in the trailing column.
- Card: `GroupBox` with title row (display name, plan), host badges, ring
  plus display-window lines, shorter windows as rows, a disclosure for pools
  and history.

### 4.8 Host badges

```text
 (laptopcomputer) This Mac      (desktopcomputer) workshop · 40 s      (wifi.slash) studio · offline 2 h
```

- Capsule, `.callout`, `.quaternary` fill, symbol plus name; age text in
  `.secondary`. Offline replaces the symbol with `wifi.slash` and adds the
  word "offline"; stale (older than three pull intervals) adds `clock` and
  "stale".
- With zero peers, badges are hidden everywhere except the Hosts section, so
  a single-Mac user never sees host noise.

### 4.9 Empty, loading and error states

- Before the projector exists, the console reads through
  `provenant ... --json` and shows no banner: that is the normal path.
- Once the projector is installed, a missing or stale digest shows a banner
  at the top of the content pane, "Showing direct reads. The projector is not
  running.", with a Start button that runs the CLI. Never a blank pane.
- Loading: `ProgressView()` only after 300 ms, so fast loads do not flash.
- CLI action failure: the CLI's typed result in an alert with the message as
  selectable text.

## 5. Colour and type tokens

### 5.1 Semantic system colours (chrome and text)

| Token | Value | Use |
|---|---|---|
| `label` | `NSColor.labelColor` | names, numbers |
| `secondaryLabel` | `secondaryLabelColor` | counts, subtitles, freshness |
| `tertiaryLabel` | `tertiaryLabelColor` | line numbers, footnotes |
| `separator` | `separatorColor` | node strokes, segment outlines under Increase Contrast |
| `track` | `quaternarySystemFill` | ring track, free space, mini bars |
| `background` | `windowBackgroundColor` (resolved `#FFFFFF` light, `#1E1E1E` dark on macOS 27) | panes, bar gaps |
| `accent` | `Color.accentColor` | selection, single-series charts, prominent buttons |
| `status.good` / `warning` / `critical` / `credit` | `.green` / `.orange` / `.red` / `.blue` | status only, always with symbol and text |

### 5.2 Categorical palette for context categories

The raw system colours fail as a categorical set: measured on macOS 27,
`systemTeal` and `systemMint` reach only about 2.1:1 against a white
background, several hues (yellow, mint, cyan) sit outside the validator's
lightness band, and `systemBrown` fails the chroma floor. So the palette
keeps the system **hues** and re-steps their lightness into a validated band.
Each colour is defined in code with
`NSColor(name:dynamicProvider:)`, choosing by
`appearance.bestMatch(from: [.aqua, .darkAqua, .accessibilityHighContrastAqua,
.accessibilityHighContrastDarkAqua])`, which works in a SwiftPM build without
an asset catalogue.

| Slot | Category family | Symbol | Hue source | Light | Dark | Light, increased contrast | Dark, increased contrast |
|---|---|---|---|---|---|---|---|
| 1 | System prompt | `text.page` | indigo | `#6C65FF` | `#7878FF` | `#6459F9` | `#8082FE` |
| 2 | System tools | `wrench.and.screwdriver` | mint | `#0A9485` | `#06A291` | `#018B7C` | `#0AAB99` |
| 3 | MCP (tools and server instructions) | `puzzlepiece.extension`, `list.bullet.rectangle` | blue | `#057FED` | `#178BFE` | `#0076DE` | `#3594FE` |
| 4 | Custom agents | `person.2` | pink | `#EC0747` | `#FB2852` | `#DE0041` | `#FE425E` |
| 5 | Memory files | `brain` | purple | `#C425D9` | `#D138E6` | `#BA11CE` | `#DA43EF` |
| 6 | Skills | `wand.and.stars` | yellow, stepped to gold | `#9C7C07` | `#AA8809` | `#917306` | `#B38F05` |
| 7 | Messages | `bubble.left.and.bubble.right` | cyan | `#078EAD` | `#039CBD` | `#0985A1` | `#08A4C6` |
| neutral | Unattributed | `questionmark.square.dashed` | hatch | `secondaryLabel` 35% | same | same, plus outline | same, plus outline |
| neutral | Autocompact buffer | `rectangle.compress.vertical` | hatch | `tertiaryLabel` | same | same, plus outline | same, plus outline |
| neutral | Free space | `square.dashed` | track | `quaternarySystemFill` | same | same, plus outline | same, plus outline |
| none | Deferred rows | `circle.dashed` | not drawn in the bar | | | | |

MCP tools and MCP server instructions share hue 3 and are told apart by
symbol and by the instructions segment being drawn at 60% of the hue; within
Messages, the drill-down sub-rows (`hammer` tool results,
`doc.text.magnifyingglass` files read, `archivebox` compaction summary) use
the Messages hue in item bars only.

Validator results (fixed order above, adjacent pairs, surfaces `#FFFFFF` and
`#1E1E1E`):

- Standard: pass in both modes. Lightness inside band, chroma at least 0.10,
  worst adjacent colour-vision separation ΔE 18.1 (protan, blue beside
  mint; target 8), worst normal-vision ΔE 18.5 (floor 15), every swatch at least
  3:1 against its background (light 3.8 to 4.6, dark 4.3 to 5.2).
- Increased contrast: pass in both modes, worst adjacent ΔE 17.0.
- Known weakness: blue beside mint is close under tritan simulation (ΔE about
  4). The 2 pt gaps, the legend and the in-segment symbols carry identity
  for that case.
- The system's own increased-contrast colour variants could not be resolved
  offline (drawing with the high-contrast appearance returned the standard
  values), so the increased-contrast steps above are computed, not sampled.
  Check them on screen with Increase Contrast on before shipping.
- Status hues (green, orange, red) are excluded from the categorical set;
  blue appears in both roles, but credit-drawing status lives only in
  Accounts and never beside the context bar.

### 5.3 Type

macOS has no Dynamic Type
([HIG Typography](https://developer.apple.com/design/human-interface-guidelines/typography)),
so the console uses system text styles throughout. Body is 13 pt.

| Role | Style |
|---|---|
| Pane title | `navigationTitle` (system toolbar title) |
| Context header ("57% used") | `.title2.weight(.semibold)`, `.monospacedDigit()` |
| Header figures ("142.0k / 250k") | `.title3`, `.monospacedDigit()`, `.secondary` for the denominator |
| Legend and table text | `.body`; numbers `.monospacedDigit()` |
| Counts, subtitles, freshness | `.callout` or `.subheadline`, `.secondary` |
| Footnote, axis labels | `.caption`, `.tertiary` |
| State text and diff | `.system(.body, design: .monospaced)` |
| Tile values | `.title.weight(.semibold)`, `.monospacedDigit()` |

Token formatting: lowercase `k` and `M` to match provider vocabulary
(`200.1k`, `1.20M`), one decimal below 1M, locale decimal separator.
Durations: "40 s", "12 m", "2 h", "4 days". Use a small custom formatter
rather than `.compactName`, which produces an uppercase `K`.

### 5.4 Spacing and shape

- 8 pt grid; 4 pt inside rows. Card padding 12 pt. Section spacing 20 pt.
- Cards and nodes 10 pt radius; menu bar panel rows 6 pt radius inside the
  panel's own radius (concentric); ring caps round. Use `ConcentricRectangle`
  for anything nested in the panel or a card edge.

## 6. Menu bar panel alignment

The panel should look like part of the same app without becoming a second
console, and it is a separate lightweight process from it.

- **No glass inside the panel.** The `.window` style panel already sits on a
  system material; adding `glassEffect` would be glass on glass. Hover rows
  keep the current `.quaternary` fill in a 6 pt rounded rectangle, which
  matches Control Center modules.
- **Same ring, same tokens.** Use the shared `Ring` (section 4.7): 40 pt
  headline, 22 pt rows, 10 pt window rows. Status hues and words identical to
  the console. Section headers in title case (Available, Drawing on Credits,
  Unavailable, Exhausted), matching macOS 26 list headers.
- **Same structure as the Accounts card**, compressed: display name, the
  display window first (weekly first), reset as fact, pace as a labelled
  estimate; disclosure for other windows. Host badges appear only when more
  than one host reports the account.
- **Width 300 pt** as today, 8 pt grid.
- **Footer menu rows:** Open Console (Command-O), Refresh (Command-R),
  Settings (Command-comma, opens the console's Settings pane), Quit
  Provenant (Command-Q, quits the menu bar agent only; an open console keeps
  running). Open Console launches the console process, which starts in
  the budget below. Text rows, not glass buttons; the HIG prefers menus
  to popovers for menu bar extras and these rows keep the menu idiom
  ([HIG The menu bar](https://developer.apple.com/design/human-interface-guidelines/the-menu-bar#Menu-bar-extras)).
- **Activity line:** one row above the footer, "5 lanes · 2 in review · 1
  needs you", read from `provenant statusline --json` until the projector
  exists and from `statusline.json` after. No extra process, no glass,
  hidden when all are zero.
- **Label:** template symbol plus monospaced percentage, as now. The label
  text changes only when its string changes; no `TimelineView` in the status
  item (the code already notes that it re-renders without end on macOS 27).

## 7. Lightweight engineering rules

### 7.1 Rules

1. **Two processes, one bundle.** A small menu bar agent and a console
   process that exits when its last window closes; no XPC service, no shared
   in-process cache, no web view. Heavy features are separate modules and
   SwiftPM targets that a build or a Settings switch can leave out, and a
   disabled module runs no timer or file watcher.
2. **Event-driven reads.** Once the projector exists, watch
   `projector/v1/manifest.json` with a `DispatchSource` file-system object
   source for `.write`, `.rename` and `.delete` (atomic replace changes the
   inode, so re-arm after rename, as the existing `ReportWatcher` does). Read
   only files whose manifest hash changed. Decode JSON off the main actor;
   apply a diff on the main actor. Until then, and whenever the digest is
   absent or stale, run `provenant ... --json` on selection or Refresh.
3. **No timers while hidden.** No repeating timer runs when no console window
   is visible and the panel is closed. Replace the always-on 60 s minute clock
   with a one-shot timer set to the next moment the menu bar label's text
   would change (a reset or exhaustion boundary), with at least 10% tolerance
   per Apple's energy guide.
4. **Relative times only while visible.** One shared minute clock starts when
   a window becomes visible and stops on close, miniaturise or full
   occlusion (`NSWindow.occlusionState`). Avoid `Text(date, style:
   .relative)`, which updates every second.
5. **Read, do not reach.** The console reads the contract when present,
   otherwise runs `provenant ... --json`. It never runs SSH and never parses
   transcripts itself. In later slices Command-R and Pull Now write a request
   file for the projector.
6. **Lazy everything.** `List` and `Table` only (both lazy on macOS); no
   `ScrollView { VStack { ForEach } }` for data rows. Session detail,
   drill-down item lists, compaction texts and diffs load on selection or
   first expansion with `.task(id:)`, and cancel when selection changes.
7. **Narrow observation.** Per-row `@Observable` models keyed by id, held in
   an `@ObservationIgnored` dictionary, so one session's change re-runs one
   row's body (WWDC25 306). Nothing that changes often goes in the
   environment.
8. **Cheap bodies.** Formatters created once; strings for numbers computed
   when data arrives, not in `body`. Target: no view body over 4 ms (half a
   120 Hz frame).
9. **Exit on close.** Closing the last console window ends the process, so
   detail caches, diff caches and chart data go with it; only the agent's menu
   bar model stays resident.
10. **Glass budget.** At most one custom `GlassEffectContainer` on screen
    (Fleet graph controls); zero custom glass in the panel.
11. **Animations are short and finite.** No repeating or indeterminate
    animation except a real in-flight action's progress indicator.
12. **Profile before shipping each slice** with the SwiftUI instrument
    (Instruments 26 template), Time Profiler and Activity Monitor's Energy
    and Idle Wake Ups columns.

### 7.2 Budgets

Measured on a reference Mac: the running menu-bar process has a **34 MB**
physical footprint (`footprint`) and showed **0.0% CPU** at idle with the
panel closed. For scale, Activity Monitor sits near 46 MB and Finder near
144 MB. Its accumulated CPU time was 23.9 s over about 50 minutes of uptime
(0.8% on average). That has not been attributed and is worth profiling
before the console is added. Budgets are per process and measured as in the
spec (Budgets and measurement).

| Process | Measure | Budget |
|---|---|---|
| Agent | Idle footprint, any module combination | at most 40 MB |
| Agent | Idle CPU | 0.0% in Activity Monitor; at most 2 s CPU per hour including refreshes |
| Agent | Idle wake-ups | at most 1 per minute averaged over 10 minutes |
| Agent | Cold launch to status item visible | at most 400 ms |
| Agent | Panel click to first frame | at most 100 ms |
| Console | Cold launch (process start) to first frame | at most 700 ms |
| Console | Data visible, 500 sessions and 200 lanes | at most 500 ms |
| Console | Sidebar section switch | at most 100 ms |
| Console | Open, steady state, 500 sessions, one Context tab | at most 120 MB footprint |
| Console | Open and idle on screen | at most 1% CPU, updates only on data change |
| Console | Hidden, minimised or occluded | no view updates at all |
| Console | Last window closed | process gone within 2 s; agent unchanged within 5 MB |
| Console | Data change applied on main actor | at most 8 ms |
| Console | Scrolling a 1,000-row Sessions list | no hitches at 120 Hz in the SwiftUI instrument |
| Both | Release binary, arm64, stripped | at most 10 MB each |

## 8. Decisions

All twelve design choices are settled as follows.

1. **Minimum macOS 26**, adopting macOS 27 refinements (`.tabs` picker,
   pointer-tuned interactive glass) behind availability checks.
2. **Settings is a sidebar pane** in the console; Command-comma jumps to it.
   No separate Settings window.
3. **Host status is a Hosts sidebar section** with a page per host.
4. **The context bar is a custom `Canvas`**, shared by list rows and the
   detail.
5. **Settings apply on commit** (System Settings style); host details edit in
   a sheet with Save, written in one call.
6. **Drill-down is per category:** every category row discloses its own
   items, with no duplicate rows.
7. **Control-Command-I toggles the inspector** (`InspectorCommands`).
8. **The menu bar panel gains one activity line**, hidden when all counts are
   zero, read from `provenant statusline --json` until the projector exists.
9. **Custom glass only for the Fleet zoom controls.**
10. **Context categories use the re-stepped palette** in section 5.2.
11. **Two processes in one bundle:** a menu bar agent and an on-demand
    console that exits with its last window, so the console's memory is freed
    and the menu bar stays light. Everything works with the menu bar off.
12. **Features are modules** the user can switch off in Settings, and a
    disabled module costs nothing at runtime.

## Appendix A. Data the design needs

From `provenant session context --json` (Slice 1) and later the projector's
session records:

- Per-category figures with their exactness label, the method
  (`provider_counted`, `probe_reconstructed`, `transcript_estimate`) and the
  estimator id, for the footnote.
- Per-item figures for skills, memory files, MCP tools (by server), built-in
  tools, agents, the ten largest tool results and files read, each with its
  exactness.
- Loaded versus deferred per tool.
- Per-turn series of the total, with breakdowns where counted, compaction
  ordinals with pre and post totals, and summary size.
- The window size and its source, and the autocompact threshold and buffer.
- Per compaction: transcript locators for the summary and for each
  `hook_success` re-injection (with the state path named in its header and
  its size), and the re-attached attachments.
- A per-session "last updated" time for the footer.

## Sources

Apple Human Interface Guidelines:
[Materials](https://developer.apple.com/design/human-interface-guidelines/materials),
[Sidebars](https://developer.apple.com/design/human-interface-guidelines/sidebars),
[Toolbars](https://developer.apple.com/design/human-interface-guidelines/toolbars),
[The menu bar](https://developer.apple.com/design/human-interface-guidelines/the-menu-bar),
[Split views](https://developer.apple.com/design/human-interface-guidelines/split-views),
[Charts](https://developer.apple.com/design/human-interface-guidelines/charts),
[Charting data](https://developer.apple.com/design/human-interface-guidelines/charting-data),
[Color](https://developer.apple.com/design/human-interface-guidelines/color),
[Accessibility](https://developer.apple.com/design/human-interface-guidelines/accessibility),
[Typography](https://developer.apple.com/design/human-interface-guidelines/typography).

Apple developer documentation:
[Adopting Liquid Glass](https://developer.apple.com/documentation/technologyoverviews/adopting-liquid-glass),
[Applying Liquid Glass to custom views](https://developer.apple.com/documentation/swiftui/applying-liquid-glass-to-custom-views),
[GlassEffectContainer](https://developer.apple.com/documentation/swiftui/glasseffectcontainer),
[BarMark](https://developer.apple.com/documentation/charts/barmark),
[Energy Efficiency Guide for Mac Apps: Timers](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/Timers.html).

WWDC sessions:
[Meet Liquid Glass (WWDC25 219)](https://developer.apple.com/videos/play/wwdc2025/219/),
[Get to know the new design system (WWDC25 356)](https://developer.apple.com/videos/play/wwdc2025/356/),
[Build a SwiftUI app with the new design (WWDC25 323)](https://developer.apple.com/videos/play/wwdc2025/323/),
[Optimise SwiftUI performance with Instruments (WWDC25 306)](https://developer.apple.com/videos/play/wwdc2025/306/),
[What's new in SwiftUI (WWDC26 269)](https://developer.apple.com/videos/play/wwdc2026/269/).

Local evidence: the macOS 27.0 SDK interfaces (SwiftUI, SwiftUICore, Charts,
AppKit `NSGlassEffectView.h`), `NSColor` values and SF Symbol names, and
`footprint` and `top` on the running menu-bar process.
