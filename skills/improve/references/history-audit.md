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

Omit unknown fields rather than guessing. Record exposure only from observed
records, not tag chronology or the current installation. Do not infer outcomes
from a transcript. Set `audit_dir` to a private temporary directory and
`skill_dir` to the installed improve skill directory, then run:

```sh
python3 "$skill_dir/scripts/audit.py" prepare "$audit_dir/manifest.json" "$audit_dir/audit"
```

See the script help for options.

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

Follow the supplied rubric. An absent next message is unknown, not an
endorsement. Check rater agreement before interpreting the labels.

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

Apply [interpretation guidance](history-interpretation.md) before making an effect claim.

## Turn findings into changes

Keep an aggregate finding table: problem, evidence, uncertainty, best-evidenced change,
owner, success measure and destination. Separate the existence of a problem
from attribution to an experiment or release. Under `tracker`, search duplicates
and file actionable tooling, documentation or doctrine issues even when no
regression is attributable; include a synthetic reproducer or proposed check.
Apply changes through the normal issue, implementation and review path. Publish
only reviewed aggregates or paraphrases, never packets, keys or raw histories.
