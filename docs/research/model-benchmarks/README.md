# Model benchmark registry

Published benchmark results per catalogue model and effort, as a machine-readable
dataset ([`benchmarks.json`](benchmarks.json), fields in
[`benchmarks.schema.json`](benchmarks.schema.json)). Retrieved 22 October 2026.

## What it is for

- **Priors for routing and effort.** An agent considering catalogue models
  or a task class's default effort reads this first to see which effort tiers
  look worth their cost and which look dominated.

## Rule: priors only

Public benchmarks order the work. They are never acceptance evidence for a
routing or effort change. Three reasons they cannot be: harness and scaffold
move cost 5 to 30 times and change rankings; public task sets leak into
training; and none of these tasks is this repository's work. A change to a
default needs the replay bench or golden tasks on our own lanes. A prior may
decide which cells to measure first, and nothing else.

Read the `contamination_note` and `run_by` of a record before leaning on it.
`vendor` rows are the model maker's own claim.

## Reading a record

`confidence` is about the figure as recorded, not the benchmark's worth: **H**
read on a primary page, independent, current; **M** primary but vendor-run, or
independent with a caveat (estimated, assumed model mapping); **L** any
secondary-only source (a blog or write-up citing a vendor or leaderboard),
unverified, or in conflict with another primary reading. `effort: null` means
the source did not state an effort; `"none"` means the source says the run used
no reasoning or effort setting (for example Snorkel's GLM-5.3-Flash row). `catalogue: false` marks a predecessor or sibling kept as a proxy; it
is not routable. `also_applies_to` lists catalogue ids assumed to share the
measurement (for example a free OpenCode route and its paid twin), which is
unverified. `cost_per_task_usd` is per benchmark task, so the AA Intelligence
Index and Terminal-Bench costs are not comparable with each other; Snorkel
Terminal-Bench cost is a run total and sits in `notes`.

## Freshness

Refresh the whole registry when a model joins or leaves the Rust catalogue at
`tools/front-door/crates/routing/assets/catalogue.toml` and otherwise each
quarter. A record's age
is `source_date`, or `retrieved` when the source is an undated live page. Treat
a record older than about six months as stale: keep it, say so when citing it,
and do not use it to override a fresher one. Next scheduled refresh: January
2027.

## Adding entries

1. Open the primary source and read the number. Never record a figure from
   memory or from a summary you did not open; if only a secondary source is
   available, record it with confidence `L` and say so in `notes`.
2. Append a record to `benchmarks.json` with every required field. Use the
   catalogue `id` as `model_id`; for a model outside the catalogue set
   `catalogue: false`. Use the effort label as the source prints it.
3. Where sources conflict, record both and explain in `notes`; do not average or
   choose silently.
4. A missing result belongs in `gaps`, not in a guess.
5. Update the summary table below, then check `benchmarks.json` against
   `benchmarks.schema.json`.

## Frontier effort range per model

Derived from the records; a prior, not a setting. "Knee" is the last tier where
the marginal cost per extra point stays low. All index figures are the Artificial
Analysis Intelligence Index (general capability, not coding) at one snapshot.

| Model | Records | Frontier range (prior) | Evidence |
|---|---|---|---|
| `claude-opus-5-5` | 11 | medium to high; xhigh as escalation | Index 42/51/54/56/58 for low to max at $0.55/1.34/1.82/3.46/5.98. AA puts medium to max on the Pareto frontier, but max costs $2.52 more than xhigh for 2 points, and Terminal-Bench 4.0 shows xhigh equal to max (59.6). |
| `claude-sonnet-5-5` | 12 | medium to high; xhigh as escalation | Index 41/47/52/56 for medium to max at $0.48/0.88/2.01/5.46. No low tier. Vendor reports max below xhigh on FrontierCode. Opus medium (51, $1.34) matches Sonnet xhigh (52, $2.01) on the index, yet Sonnet leads Terminal-Bench 4.0 (63.6 vs 59.6). |
| `claude-haiku-5-5` | 8 | medium to xhigh on cost; low evidence for coding | Index 29/34/38/41/43 for low to max at $0.02 to $0.21, so cost is small at every tier, but xhigh emits 180M index tokens. Terminal-Bench 4.0 39.2 (vendor, effort unstated) against Sonnet 5.5 at 70.6. |
| `gpt-6.1-sol` | 9 | medium to high; xhigh marginal | Index 42/48/50/51/52 for low to max at $0.13/0.21/0.32/0.39/0.72. Max adds 1 point for $0.33 over xhigh. DeepSWE high 75.2 at about $1.50 (vendor). |
| `gpt-6-luna` | 6 | high to max; xhigh unmeasured | Index 22/30/33/38 for low/medium/high/max at $0.0045 to $0.07. Cheap everywhere. Max (38) is below Sol low (42). Terminal-Bench 4.0 16.4 at max. |
| `gpt-6-astra` | 3 | not established (max only) | Index 53 at $3.26 against Sol 6.1 max 52 at $0.72; Terminal-Bench 4.0 58.2 equal to Sol 6.1. DeepSWE high 74.8 at about $7.70 against Sol 6.1 75.2 at about $1.50. |
| `gemini-3.8-flash` | 7 | medium | Sources conflict. Live pages: 33/40/41 for low/medium/high, $0.93 and $1.24 at medium and high. Launch article: 52/57/59, $0.24/0.41/0.58. Both show medium within 1 to 2 points of high and low 5 to 7 points down. Terminal-Bench 4.0 19.1 at high. |
| `grok-4.7` | 10 | medium to high | CursorBench 4.0 (vendor) 33.1/41.6/43.9/46.3 for low to xhigh at $1.58/3.49/4.69/6.01. Index: high 46 equals xhigh 46 for $1 more. Terminal-Bench 4.0 37.6 at xhigh. |
| `cursor-grok-4.6` | 3 | not established (no effort control in catalogue) | Proxy mapping from Grok 4.6 high: index 44, Terminal-Bench 4.0 20.3, DeepSWE 65.2. |
| `claude-fable-5-1` | 2 | capped at medium by the catalogue | Only max measured: index 53 at $7.63; Terminal-Bench 4.0 57.9. |
| `opencode-go/deepseek-v4.1-flash` (and free route) | 3 | no effort axis | Index 39 at $0.40 (a newer checkpoint lists 45). Terminal-Bench 4.0 42.3 (vendor, secondary). |
| `opencode-go/glm-5.3-flash` (and free route) | 2 | no effort axis | Index 42 at $0.28. Terminal-Bench 4.0 35.8. |
| `opencode-go/gemini-4-argon-contributor` | 1 | not established | Index 53 at $1.99 at high (mapping assumed). |
| `opencode/mimo-v2.6-flash-free` | 1 | no effort axis | Index 38 at $0.06. |
| `claude-opus-4-6-thinking`, `claude-sonnet-4-6`, `claude-haiku-4-5` (agy and legacy) | 2, 1, 2 | no effort axis | Deprecated. Estimated index 32 and 25; Scale SWE-bench Pro 51.9 (Opus 4.6) and 39.45 (Haiku 4.5). |
| `gemini-3.7-flash` | 1 | not established | Terminal-Bench 4.0 11.2 (high). |
| `composer-2.5`, `auto`, `opencode/longcat-2.5-preview-free` | 0 | none | No benchmark found; see `gaps`. |

Records for proxies outside the catalogue (`catalogue: false`: Grok 4.5, Fable 5,
Opus 5, Sonnet 5, GPT-5.6 Sol and Luna, GLM-5.3) are counted separately in
`benchmarks.json` and carry the SWE-rebench May to July 2026 window.

## Main gaps

- No independent per-effort coding result (SWE-bench Pro, SWE-rebench, LiveCodeBench,
  Aider polyglot) for any current-generation model. SWE-rebench has only the
  predecessors, LiveCodeBench loaded no data, and Aider's newest entry is October
  2025. The per-effort curves come from the AA Intelligence Index, which is not a
  coding index, plus vendor curves (Grok 4.7 CursorBench).
- The Gemini 3.8 Flash figures conflict by about 18 index points between the
  launch article and the live pages. Unreconciled.
- Opus 5.5 low appears only as an index and one Terminal-Bench figure; Haiku 5.5
  has no independent coding score; Luna xhigh and Grok 4.7 medium (index) are
  missing; Composer, Auto and LongCat have nothing.
- Leaderboard harnesses differ (mini-swe-agent, Claude Code, Codex, Grok Build);
  Terminal-Bench 4.0 for Sonnet 5.5 reads 63.6, 61.8 or 70.6 by source.
- Free OpenCode routes are assumed equal to their paid twins; unverified.
- The paper arXiv 2608.01347, "Prompt-Induced Waste in Coding Agents", was not
  readable. Its qualitative finding, that more effort can add cost with no gain
  and that harness shifts cost per success 5 to 30 times, matches the rule above
  but is not recorded as data.

Qualitative judgement about each model stays in
[`docs/model-dossier.md`](../../model-dossier.md); this registry holds only
numbers with their sources.
