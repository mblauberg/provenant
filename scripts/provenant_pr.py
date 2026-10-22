#!/usr/bin/env python3
"""Pull-request helpers over gh: block on a PR's required checks."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time

DEFAULT_TIMEOUT = 1800.0
BACKOFF_START = 30.0
BACKOFF_CAP = 120.0
BACKOFF_FACTOR = 1.5
GH_TIMEOUT = 60
# 0 green, 1 failure, 3 still pending (no --wait), 4 required set unknown, 124 timeout.
EXIT = {"pass": 0, "fail": 1, "pending": 3, "unknown": 4}
PASSING = {"pass", "skipping"}
FAILING = {"fail", "cancel"}


def backoff_bounds() -> tuple[float, float]:
    """Poll interval start and cap; PROVENANT_PR_BACKOFF=start,cap lets tests run fast."""
    raw = os.environ.get("PROVENANT_PR_BACKOFF")
    if raw:
        try:
            start, cap = (float(part) for part in raw.split(","))
            return start, cap
        except ValueError:
            pass
    return BACKOFF_START, BACKOFF_CAP


def gh(*arguments: str) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(["gh", *arguments], capture_output=True, text=True, timeout=GH_TIMEOUT)
    except (OSError, subprocess.SubprocessError) as exc:
        return subprocess.CompletedProcess(["gh", *arguments], 127, "", str(exc))


def view(reference: str) -> dict | None:
    result = gh("pr", "view", reference, "--json", "number,headRefOid,state,baseRefName")
    if result.returncode != 0:
        view.error = result.stderr.strip() or "gh pr view failed"  # type: ignore[attr-defined]
        return None
    try:
        data = json.loads(result.stdout)
    except json.JSONDecodeError:
        view.error = "gh pr view returned invalid JSON"  # type: ignore[attr-defined]
        return None
    return data if isinstance(data, dict) else None


def parse_checks(stdout: str) -> list[dict] | None:
    try:
        data = json.loads(stdout)
    except json.JSONDecodeError:
        return None
    if not isinstance(data, list):
        return None
    return [
        {"name": str(row.get("name", "")), "bucket": str(row.get("bucket") or "pending"),
         "link": row.get("link") or ""}
        for row in data
        if isinstance(row, dict)
    ]


def fetch_checks(number: int) -> list[dict] | None:
    """Required checks for the PR's current head; all checks when none are required.

    None means gh could not answer. An empty list means nothing is registered yet.
    """
    fields = "name,bucket,link"
    required = gh("pr", "checks", str(number), "--required", "--json", fields)
    rows = parse_checks(required.stdout)
    if rows is None:
        message = (required.stderr or "").lower()
        if "no required checks" not in message:
            return [] if "no checks reported" in message else None
        every = gh("pr", "checks", str(number), "--json", fields)
        rows = parse_checks(every.stdout)
        if rows is None:
            return [] if "no checks reported" in (every.stderr or "").lower() else None
    return rows


def expected_contexts(base: str) -> set[str] | None:
    """Required status-check contexts for the base branch, from rulesets and branch protection.

    None means either source could not be read; an empty set means both answered
    and nothing is required. Classic protection answering 404 means the branch is
    unprotected, which is an answer; any other failure leaves the set unknown.
    """
    def read(path: str, absent_ok: bool, contexts) -> set[str] | None:
        result = gh("api", path)
        if result.returncode != 0:
            return set() if absent_ok and "HTTP 404" in (result.stderr or "") else None
        try:
            return {str(name) for name in contexts(json.loads(result.stdout))} - {"None"}
        except (json.JSONDecodeError, AttributeError, TypeError):
            return None

    def ruled(listed):
        if not isinstance(listed, list):
            raise TypeError("rules response is not a list")
        for rule in listed:
            if isinstance(rule, dict) and rule.get("type") == "required_status_checks":
                for check in (rule.get("parameters") or {}).get("required_status_checks") or []:
                    yield check.get("context")

    def classic(data):
        yield from data.get("contexts") or []
        yield from (check.get("context") for check in data.get("checks") or [])

    rules = read(f"repos/{{owner}}/{{repo}}/rules/branches/{base}", False, ruled)
    protection = read(f"repos/{{owner}}/{{repo}}/branches/{base}/protection/required_status_checks", True, classic)
    return None if rules is None or protection is None else rules | protection


def finished(rows: list[dict]) -> bool:
    return all(row["bucket"] in PASSING | FAILING for row in rows)


def judge(rows: list[dict], expected: set[str] | None, previous: list[str] | None) -> tuple[list[dict], str]:
    """Rows that count, and pending, pass, fail or unknown.

    Known required contexts match check names exactly and must all be present.
    An empty or unknown required set cannot see a check that has not registered
    yet, so it needs the finished set to be unchanged since the previous poll;
    an unknown set then reports "unknown", never "pass".
    """
    if expected:
        rows = [row for row in rows if row["name"] in expected]
        complete = expected <= {row["name"] for row in rows}
        if not (rows and complete and finished(rows)):
            return rows, "pending"
    elif not (rows and finished(rows) and previous == sorted(row["name"] for row in rows)):
        return rows, "pending"
    if any(row["bucket"] in FAILING for row in rows):
        return rows, "fail"
    return rows, "unknown" if expected is None else "pass"


def emit(number: int, head: str, outcome: str, rows: list[dict], as_json: bool) -> None:
    if as_json:
        print(json.dumps({"number": number, "head": head, "outcome": outcome, "checks": rows}, sort_keys=True))
        return
    for row in rows:
        print(f"{row['bucket']:<8} {row['name']}")
    print(f"pr #{number} {head[:12]}: {outcome}")


def checks(args: argparse.Namespace) -> int:
    start, cap = backoff_bounds()
    deadline = time.monotonic() + args.timeout
    state = view(args.reference)
    if state is None:
        print(f"provenant pr: {view.error}", file=sys.stderr)  # type: ignore[attr-defined]
        return 2
    number = int(state["number"])
    head = str(state["headRefOid"])
    rows: list[dict] = []
    expected = expected_contexts(str(state.get("baseRefName") or "main"))
    previous: list[str] | None = None
    delay = start
    while True:
        if state is not None:
            if state.get("state") != "OPEN":
                emit(number, head, f"pr is {str(state.get('state')).lower()}", rows, args.json)
                return 1
            if state["headRefOid"] != head:
                print(f"provenant pr: head moved to {state['headRefOid'][:12]}; following it", file=sys.stderr)
                head, rows, delay, previous = str(state["headRefOid"]), [], start, None
        fetched = fetch_checks(number)
        after = view(str(number))
        # A push between the two calls makes the rows belong to the old head; discard them.
        if fetched is not None and after is not None and after["headRefOid"] == head:
            counted, outcome = judge(fetched, expected, previous)
            rows = counted
            previous = sorted(row["name"] for row in fetched) if fetched and finished(fetched) else None
            if not args.wait and outcome == "pending" and not expected and fetched and finished(fetched):
                outcome = "fail" if any(row["bucket"] in FAILING for row in fetched) else (
                    "unknown" if expected is None else "pass")
            if not args.wait or outcome != "pending":
                if outcome == "unknown":
                    print("provenant pr: all seen checks passed, but the required set could not be read "
                          "(no access to rulesets or branch protection); not reporting success", file=sys.stderr)
                emit(number, head, outcome, rows, args.json)
                return EXIT[outcome]
        state = after
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            emit(number, head, "timeout", rows, args.json)
            return 124
        time.sleep(min(delay, remaining))
        delay = min(delay * BACKOFF_FACTOR, cap)
        state = view(str(number)) or state


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(prog="provenant pr")
    commands = root.add_subparsers(dest="command", required=True)
    command = commands.add_parser("checks", help="report or wait for a PR's required checks")
    command.add_argument("reference", help="PR number or branch")
    command.add_argument("--wait", action="store_true")
    command.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT, metavar="S")
    command.add_argument("--json", action="store_true")
    return root


def main(argv: list[str]) -> int:
    args = parser().parse_args(argv)
    if args.timeout < 0:
        print("provenant pr: --timeout must not be negative", file=sys.stderr)
        return 2
    return checks(args)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
