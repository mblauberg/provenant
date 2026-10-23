#!/usr/bin/env python3
"""Compare two front doors using fresh golden fixtures and shared class rules."""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import difflib
import json
from itertools import islice
import os
from pathlib import Path
import re
import sys
from typing import Any

from run import HERE, REPO, Case, all_cases, binary_path, comparison_fields, run_case


LIMIT = 2000
MISSING = {"missing": True}


def as_text(value: Any) -> str:
    return value if isinstance(value, str) else json.dumps(value, sort_keys=True, indent=2)


def bounded(value: Any) -> Any:
    text = as_text(value)
    return value if len(text) <= LIMIT else text[:LIMIT] + f"… [{len(text)} chars]"


def short_diff(a: Any, b: Any) -> str:
    lines = difflib.unified_diff(as_text(a).splitlines(), as_text(b).splitlines(),
                                 fromfile="a", tofile="b", n=2, lineterm="")
    # Bound both line count and line length (JSON output may be one huge line).
    return "\n".join(line[:240] for line in islice(lines, 30))


def load_allowlist(path: Path) -> list[dict[str, str]]:
    entries = json.loads(path.read_text())
    if not isinstance(entries, list):
        raise ValueError("allowlist must be a JSON array")
    for entry in entries:
        if not isinstance(entry, dict) or any(not isinstance(entry.get(key), str) or not entry[key].strip()
                                              for key in ("case", "field", "reason")):
            raise ValueError("each allowlist entry needs nonempty case, field and reason strings")
        if set(entry) - {"case", "field", "reason", "a", "b"}:
            raise ValueError(f"unknown allowlist keys: {sorted(set(entry) - {'case', 'field', 'reason', 'a', 'b'})}")
        for side in ("a", "b"):
            if side in entry:
                re.compile(entry[side])
    return entries


def allowed_by(entry: dict[str, str], case: Case, field: str, a: Any, b: Any) -> bool:
    return (entry["case"] == case.name and entry["field"] == field
            and all(side not in entry or re.search(entry[side], as_text(value)) is not None
                    for side, value in (("a", a), ("b", b))))


def case_fields(binary: Path, case: Case, env: dict[str, str], checkout_excluded: tuple[Path, ...] = ()) -> dict[str, Any]:
    try:
        return comparison_fields(run_case(binary, case, env, checkout_excluded=checkout_excluded))
    except Exception as exc:
        # Infrastructure/setup errors must never become a vacuous match.
        return {"execution_error": f"{type(exc).__name__}: {exc}"}


def compare_case(case: Case, a: Path, b: Path, env_a: dict[str, str], env_b: dict[str, str],
                 checkout_excluded: tuple[Path, ...] = ()):
    left = case_fields(a, case, env_a, checkout_excluded)
    right = case_fields(b, case, env_b, checkout_excluded)
    fields = []
    for field in sorted(left.keys() | right.keys()):
        av, bv = left.get(field, MISSING), right.get(field, MISSING)
        if av != bv or field == "execution_error":
            fields.append((field, av, bv))
    return case, fields


def positive(value: str) -> int:
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return number


def sides(args, parser):
    a = args.a or os.environ.get("PROVENANT_A")
    b = args.b or os.environ.get("PROVENANT_B")
    env_a: dict[str, str] = {}
    env_b: dict[str, str] = {}
    if args.impl_env:
        if args.b:
            parser.error("--impl-env uses one binary; supply --a only")
        match = re.fullmatch(r"([A-Za-z_][A-Za-z0-9_]*)=([^,=]+),([^,=]+)", args.impl_env)
        if not match:
            parser.error("--impl-env must be NAME=value_a,value_b")
        name, va, vb = match.groups()
        if va == vb:
            parser.error("--impl-env requires two distinct implementation values")
        a = a or binary_path()
        b = a
        env_a[name], env_b[name] = va, vb
    if not a or not b:
        parser.error("supply --a BIN --b BIN (or PROVENANT_A / PROVENANT_B), or --impl-env")
    paths = [Path(value).expanduser().resolve() for value in (a, b)]
    for path in paths:
        if not path.is_file() or not os.access(path, os.X_OK):
            parser.error(f"front-door binary is missing or not executable: {path}")
    return *paths, env_a, env_b


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--a", metavar="BIN")
    parser.add_argument("--b", metavar="BIN")
    parser.add_argument("--impl-env", metavar="NAME=VALUE_A,VALUE_B")
    parser.add_argument("--verb", action="append", help="select a verb; repeat to select more")
    parser.add_argument("--case", action="append", help="select a case name; repeat to select more")
    parser.add_argument("--jobs", type=positive, default=1)
    parser.add_argument("--allowlist", type=Path, default=HERE / "diff-allow.json")
    parser.add_argument("--output-dir", type=Path, help="exact report directory (default: .agent-run/diff/<UTC timestamp>)")
    args = parser.parse_args()
    a, b, env_a, env_b = sides(args, parser)
    cases = all_cases()
    for option, values, known in (("--verb", args.verb, {c.verb for c in cases}),
                                  ("--case", args.case, {c.name for c in cases})):
        unknown = set(values or []) - known
        if unknown:
            parser.error(f"unknown {option}: {', '.join(sorted(unknown))}")
    cases = [c for c in cases if (not args.verb or c.verb in args.verb)
             and (not args.case or c.name in args.case)]
    if not cases:
        parser.error("filters selected no cases")
    try:
        allowlist = load_allowlist(args.allowlist)
    except (OSError, ValueError, TypeError, re.error) as exc:
        parser.error(f"invalid allowlist: {exc}")
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    output = args.output_dir or REPO / ".agent-run" / "diff" / stamp
    output.mkdir(parents=True, exist_ok=False)
    summary: dict[str, Any] = {"cases": len(cases), "divergences": 0, "allowed": 0,
                               "unexpected": 0, "per_verb": {}, "a": str(a), "b": str(b),
                               "env_a": env_a, "env_b": env_b}
    matched: set[int] = set()
    with ThreadPoolExecutor(max_workers=args.jobs) as pool, (output / "divergences.jsonl").open("w") as log:
        results = pool.map(lambda case: compare_case(case, a, b, env_a, env_b, (output,)), cases)
        for case, divergences in results:
            counts = summary["per_verb"].setdefault(case.verb, {"cases": 0, "divergences": 0, "allowed": 0, "unexpected": 0})
            counts["cases"] += 1
            for field, av, bv in divergences:
                allowed = False
                reason = None
                for index, entry in enumerate(allowlist):
                    if field != "execution_error" and allowed_by(entry, case, field, av, bv):
                        matched.add(index)
                        allowed, reason = True, entry["reason"]
                        break
                row = {"case": case.name, "verb": case.verb, "field": field,
                       "a": bounded(av), "b": bounded(bv), "allowed": allowed,
                       "diff": short_diff(av, bv)}
                if reason:
                    row["reason"] = reason
                log.write(json.dumps(row, sort_keys=True) + "\n")
                for counts_set in (summary, counts):
                    counts_set["divergences"] += 1
                    counts_set["allowed" if allowed else "unexpected"] += 1
    stale = [entry for index, entry in enumerate(allowlist) if index not in matched]
    summary["stale_allowlist"] = stale
    (output / "summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    for entry in stale:
        print(f"warning: stale allowlist entry {entry['case']} / {entry['field']}: {entry['reason']}", file=sys.stderr)
    print(f"{len(cases)} cases: {summary['unexpected']} unexpected, {summary['allowed']} allowed divergence(s); {output}")
    return 1 if summary["unexpected"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
