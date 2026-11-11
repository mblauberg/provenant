# Local history audit

Run after a harness experiment, release, instruction change or a long run with
repeated objections. The question is what can be improved and what can be
compared honestly. A detectable regression is not a prerequisite for a finding.

## Prepare selected histories

The scripts are in this skill's `scripts/` directory and use only Python 3's
standard library. They do not discover home directories, modify source
histories, call providers or file issues.
Read the authorised local files in place. Keep intermediate files local, outside
Git. Do not select credential stores or files containing secrets for model input.

Create a private manifest with explicit sources at `$audit_dir/manifest.json`.
Paths below are placeholders:

```json
{
  "sources": [
    {"host": "claude", "path": "local/claude.jsonl", "session": "synthetic-chair", "project": "local-project", "work_mix": "implementation"},
    {"host": "codex", "path": "local/codex.jsonl"},
    {"host": "lane", "path": "local/stdout.jsonl", "session": "synthetic-attempt", "sample": "synthetic-node", "graph": "synthetic-graph"}
  ],
  "releases": [{"timestamp": "2030-01-01T00:00:00Z", "release": "synthetic-tag"}],
  "outcomes": "local/outcomes.json"
}
```

Omit optional fields rather than guessing. Supply project and work mix only from
observed records or a documented local classification; compare alternative work
classifications when judgement is uncertain. Native Codex metadata supplies its
session identity. Claude uses the recorded session/message identities; lanes use
the provider transcript or Codex exec JSON events and the attempt identity.
Duplicate native messages count once. Tool-step usage contributes to the reply
ending that turn; Claude lane result totals supersede intermediate counters.
Token counters retain unknown components. Source totals
(`input_total` and `cache_creation_total`) remain available when missing cache
splits prevent disjoint token categories from being measured. Their aggregate
`observed_total` counts only measured replies; do not add overlapping source
totals to the disjoint categories.

Sources can include `experiment`, `installed_release`, `instruction_version`,
`context`, and timestamped `exposures`, each with a `timestamp` and changed fields.
These describe **recorded** exposure: never fill them from tag chronology or the
current installation. A missing field stays null. Native captured instructions
produce `captured_instruction_hash`, identifying only material captured so far;
it is not a complete installed instruction version. `release_era_proxy` comes
only from the supplied tag timeline and is not an installed release. Exposure
changes increment `within_session_era`; compaction boundaries are separate.

Optional `outcomes` is the existing telemetry outcome export. The reader joins
its `evidence.sample` and, when supplied, `graph`, preserving the attribution,
maturity and unmeasured states owned by `outcome.rs`. It never guesses a merge
from a transcript or duplicates that attribution algorithm.

```sh
audit_dir="$(mktemp -d)"
skill_dir=/path/to/installed/improve   # this skill's directory
# Create "$audit_dir/manifest.json" using the template above.
python3 "$skill_dir/scripts/audit.py" prepare "$audit_dir/manifest.json" "$audit_dir/audit" --seed 7 --audit-count 30
```

Keep the manifest and output outside the checkout because they contain private
paths and transcript excerpts.
`rows.json` and `key.json` are private. `blind.json` and `audit-blind.json` contain
only shuffled opaque IDs and user/reply/next-user excerpts. Review these packets
before dispatch: mask explicit group, date, release, project and instruction
mentions in the excerpts, and remove secrets. Metadata stripping cannot blind a
self-identifying sentence. Keep IDs aligned and record masking locally. Do not
send the rows, manifest or key to raters.

## Blind panel

Give a cheap model `rubric.txt` and `blind.json`; save its JSON array as
`cheap.json`. Independently give a stronger model the same rubric and
`audit-blind.json`; save `strong.json`. Select actual configured models with
`orchestrate`, and record their resolved routes locally. Do not show the cheap
labels to the stronger auditor. The randomly selected audit subset is fixed by
the seed and count; increase coverage when raters disagree or events are rare.

Labelling covers reliance on documents or existing code, whether the reply
examines their reason, generic change-cost arguments, and the next message's
objection. An absent next message is unknown, not an endorsement. The schema
rejects incomplete labels and prints aggregate agreement and Cohen's kappa.

Raw history may go to other providers only when the owner authorised that run;
credentials and secrets are excluded in every case. Otherwise use an authorised
local model, or keep the packets local and report labelling as unverified.
Local read-only history analysis needs no receipt.

```sh
python3 "$skill_dir/scripts/audit.py" report "$audit_dir/audit" --cheap "$audit_dir/cheap.json" --audit "$audit_dir/strong.json" > "$audit_dir/report.json"
python3 "$skill_dir/scripts/audit.py" lenses "$audit_dir/report.json" > "$audit_dir/lenses.json"
```

The report contains aggregates only: no transcript text, project names, native
identities, timestamps or free-form rater explanations. Dimension values use
ordinal aliases; the local rows hold their interpretation. Assign the aggregate
cause, reasoning and change packets to multiple configured models; synthesise
their conclusions against the estimates and coverage, never by majority vote.

## Read the comparison

Each pair compares the second level with the first, sorted by its source value.
The estimator is a session-balanced linear probability comparison: each
session's turns together have weight one. It adjusts for project, calendar day,
work mix, context and compaction, plus other observed exposure dimensions,
except the derived within-session era and the dimension being compared.
Numeric context stays linear when some measurements are missing, with a
separate missingness indicator. Other controls are categorical. Missing controls
form explicit unknown levels and are counted. This does not adjust for
unrecorded causes. Tight daily controls can leave historical comparisons
unestimable; widen periods only with a stated sensitivity analysis.

Zero joint categorical overlap or rank loss reports `non_identifiable`: the
data cannot separate group from the controls. Otherwise `identifiable` means an estimable adjusted association,
not proof of causation. Both raw and adjusted differences are returned.

Uncertainty uses whole-session residual sign permutations, exactly enumerated
for up to ten sessions and seeded Monte Carlo above that. `permutation_interval`
is the null score interval, not a confidence interval. `confidence_interval`
is the estimate shifted by full-fit residual sign permutations; it is an
approximate interval, without studentisation or leverage correction. It relies
on independent sessions and symmetric session residuals; serial sessions from
one project can violate that assumption. Neither interval treats turns as
independent observations. Leave-one-session-out reports the range, sign flips
and losses of identifiability.

Read session counts, joint-overlap sessions, excluded replies, missing controls,
audit agreement and leave-one-out sensitivity before any effect claim. A few sessions, weak overlap,
rare outcomes, a wide interval or one influential session means inconclusive.
Report all pairwise comparisons as exploratory; there is no multiple-testing
correction. A null or non-identifiable result does not establish equivalence.

## Turn findings into changes

Keep an aggregate finding table: problem, evidence, uncertainty, smallest change,
owner, success measure and destination. Separate the existence of a problem
from attribution to an experiment or release. Under `tracker`, search duplicates
and file actionable tooling, documentation or doctrine issues even when no
regression is attributable; include a synthetic reproducer or proposed check.
Apply changes through the normal issue, implementation and review path. Publish
only reviewed aggregates or paraphrases, never packets, keys or raw histories.
