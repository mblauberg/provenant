#!/usr/bin/env python3
"""Verify one implementation candidate at the ordinary host-chair boundary."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
from typing import Any


# This helper is installed with the harness.  It must never infer a verifier
# from the project under review: that would let a candidate choose its own
# custody code.
ROOT = Path(__file__).resolve().parents[3]
WORKTREE_SCRIPT = ROOT / "scripts" / "worktree.py"
RECEIPT_NAME = "implementation.worktree.json"
RESULT_NAME = "implementation.acceptance.json"
COMMIT_SHA = re.compile(r"^[0-9a-fA-F]{40}(?:[0-9a-fA-F]{24})?$")

if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from scripts import worktree as worktree_policy  # noqa: E402


def digest(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def fixed_output_path(run_dir: Path, name: str) -> Path:
    path = run_dir / name
    if path.is_symlink():
        raise ValueError(f"fixed output {name} must not be a symlink")
    if path.exists() and (not path.is_file() or path.stat().st_nlink != 1):
        raise ValueError(f"fixed output {name} must be a unique regular file")
    return path


def existing_directory(value: Path, label: str) -> Path:
    if value.is_symlink():
        raise ValueError(f"{label} must not be a symlink")
    try:
        resolved = value.expanduser().resolve(strict=True)
    except (OSError, RuntimeError) as exc:
        raise ValueError(f"{label} must be an existing directory") from exc
    if not resolved.is_dir():
        raise ValueError(f"{label} must be an existing directory")
    return resolved


def _valid_commit(value: object, label: str) -> str:
    if not isinstance(value, str) or not COMMIT_SHA.fullmatch(value):
        raise ValueError(f"{label} must be a full Git object ID")
    return value


def verify_claim(args: argparse.Namespace, canonical: Path, claimed: Path) -> dict[str, Any]:
    """Run only the installed worktree verifier and preserve its observed exit."""
    claimed_commit = _valid_commit(args.claimed_commit, "claimed commit")
    base_revision = _valid_commit(args.base_revision, "base revision")
    try:
        expected_common = worktree_policy.common_git_dir(canonical)
    except worktree_policy.PolicyError as exc:
        return {"status": "rejected", "reason": str(exc), "verifier_exit": 2}

    command = [
        sys.executable,
        str(WORKTREE_SCRIPT),
        "verify-claim",
        "--repo", str(canonical),
        "--claimed-worktree", str(claimed),
        "--claimed-commit", claimed_commit,
        "--base-revision", base_revision,
        "--expected-common", str(expected_common),
    ]
    observed = subprocess.run(
        command, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
    )
    if observed.returncode != 0:
        return {
            "status": "rejected",
            "reason": observed.stderr.strip() or "installed worktree verifier rejected the claim",
            "verifier_exit": observed.returncode,
        }
    try:
        receipt = json.loads(observed.stdout)
    except json.JSONDecodeError:
        return {"status": "rejected", "reason": "installed worktree verifier returned invalid JSON", "verifier_exit": observed.returncode}
    if not isinstance(receipt, dict):
        return {"status": "rejected", "reason": "installed worktree verifier returned a non-object", "verifier_exit": observed.returncode}

    expected = {
        "status": "accepted",
        "claimed_worktree": str(canonical),
        "common_git_dir": str(expected_common),
        "base_revision": base_revision,
        "head_revision": claimed_commit,
        "claimed_commit": claimed_commit,
        "clean": True,
    }
    for field, expected_value in expected.items():
        if receipt.get(field) != expected_value:
            return {
                "status": "rejected",
                "reason": f"installed worktree verifier receipt does not bind exact {field}",
                "verifier_exit": observed.returncode,
            }
    receipt["verifier_exit"] = observed.returncode
    return receipt


def accept(args: argparse.Namespace) -> int:
    run_dir: Path | None = None
    result: dict[str, object]
    try:
        run_dir = existing_directory(Path(args.run_dir), "run-dir")
        canonical = existing_directory(Path(args.canonical_worktree), "canonical worktree")
        claimed_value = getattr(args, "claimed_worktree", None)
        claimed = canonical if claimed_value is None else existing_directory(Path(claimed_value), "claimed worktree")
        claim_receipt = verify_claim(args, canonical, claimed)
        receipt_path = fixed_output_path(run_dir, RECEIPT_NAME)
        write_json(receipt_path, claim_receipt)
        worktree_ref = {"path": RECEIPT_NAME, "digest": digest(receipt_path)}
        if claim_receipt.get("status") != "accepted":
            result = {"status": "rejected", "reason": claim_receipt.get("reason", "claim rejected")}
        else:
            result = {
                "status": "accepted",
                "result_revision": claim_receipt["head_revision"],
                "worktree_receipt": worktree_ref,
            }
    except (OSError, TypeError, ValueError, worktree_policy.PolicyError) as exc:
        result = {"status": "rejected", "reason": str(exc)}

    if run_dir is None:
        print(json.dumps(result, sort_keys=True))
        return 1
    try:
        result_path = fixed_output_path(run_dir, RESULT_NAME)
        write_json(result_path, result)
    except (OSError, ValueError) as exc:
        print(json.dumps({"status": "rejected", "reason": str(exc)}, sort_keys=True))
        return 1
    result_for_output = dict(result)
    result_for_output["acceptance_result"] = {
        "path": RESULT_NAME,
        "digest": digest(result_path),
    }
    if "worktree_receipt" not in result:
        try:
            receipt_path = fixed_output_path(run_dir, RECEIPT_NAME)
        except (OSError, ValueError):
            receipt_path = None
        if receipt_path is not None and receipt_path.is_file():
            result_for_output["worktree_receipt"] = {
                "path": RECEIPT_NAME,
                "digest": digest(receipt_path),
            }
    print(json.dumps(result_for_output, sort_keys=True))
    return 0 if result.get("status") == "accepted" else 1


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--canonical-worktree", type=Path, required=True)
    result.add_argument("--claimed-worktree", type=Path)
    result.add_argument("--claimed-commit", required=True)
    result.add_argument("--base-revision", required=True)
    result.add_argument("--run-dir", type=Path, required=True)
    return result


if __name__ == "__main__":
    raise SystemExit(accept(parser().parse_args()))
