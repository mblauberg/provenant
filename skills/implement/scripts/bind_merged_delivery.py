#!/usr/bin/env python3
"""Bind live GitHub and exact-head review evidence into a software RUN.json."""

from __future__ import annotations

import argparse
import fcntl
from functools import lru_cache
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
from typing import Any


PRODUCT_ROOT = Path(
    os.environ.get("AGENT_FABRIC_PRODUCT_ROOT", Path(__file__).resolve().parents[3])
).expanduser()
SKILLS_ROOT = Path(__file__).resolve().parents[2]


def fail(condition: bool, message: str) -> None:
    if condition:
        raise ValueError(message)


def command(*argv: str, text: bool = True) -> str | bytes:
    return subprocess.run(argv, check=True, capture_output=True, text=text).stdout


@lru_cache(maxsize=1)
def git_evidence():
    path = PRODUCT_ROOT / "scripts/git_evidence.py"
    spec = importlib.util.spec_from_file_location("binder_git_evidence", path)
    fail(not spec or not spec.loader, "canonical Git evidence runner is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def git(root: Path, *args: str, text: bool = True) -> str | bytes:
    return git_evidence().git_output(root, *args, text=text)


def github(path: str) -> dict[str, Any]:
    raw = command("gh", "api", "--method", "GET", path)
    assert isinstance(raw, str)
    value = json.loads(raw)
    fail(not isinstance(value, dict), f"GitHub API {path} returned a non-object")
    return value


def artifact(path: str, artifact_id: str, raw: bytes) -> dict[str, Any]:
    return {
        "id": artifact_id, "path": path, "media_type": "application/json",
        "artifact_type": "evidence", "digest": "sha256:" + hashlib.sha256(raw).hexdigest(),
        "class": "evidence", "owner": "delivery-chair", "retention": "risk-policy",
    }


def encode(value: dict[str, Any]) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def delivery_validator():
    path = SKILLS_ROOT / "deliver/scripts/validate_delivery.py"
    spec = importlib.util.spec_from_file_location("bind_merged_delivery_validator", path)
    fail(not spec or not spec.loader, "delivery validator is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def preflight(
    run: dict[str, Any], receipt: Path, workspace: Path,
    validator: Any, product_root: Path,
) -> None:
    fail(not isinstance(run, dict) or run.get("contract") != "delivery-run" or run.get("schema_version") != 1,
         "receipt must be a delivery-run v1 object")
    project_policy = run.get("project_policy") if isinstance(run.get("project_policy"), dict) else {}
    policy_path = workspace / project_policy["path"] if project_policy.get("path") else None
    validator.validate(
        run, product_root, receipt_dir=receipt.parent, workspace_root=workspace,
        project_policy_path=policy_path, verify_hashes=True,
    )
    authority = run["authority"]
    network = authority["network"]
    fail(network.get("tool_egress") != "allowlist" or "api.github.com" not in network.get("allowed_hosts", []),
         "receipt authority must allowlist api.github.com tool egress")
    fail(authority.get("secrets_access") != "use-without-disclosure"
         or "github-cli-auth" not in authority.get("secret_refs", []),
         "receipt authority must permit the github-cli-auth credential reference")


def main(argv: list[str] | None = None) -> int:
    global PRODUCT_ROOT
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("receipt", type=Path)
    parser.add_argument("--workspace-root", type=Path, required=True)
    parser.add_argument("--repository", required=True, help="GitHub owner/repository")
    parser.add_argument("--pr-number", type=int, required=True)
    parser.add_argument("--review-artifact", type=Path, action="append", required=True)
    parser.add_argument("--product-root", type=Path, default=PRODUCT_ROOT)
    args = parser.parse_args(argv)
    PRODUCT_ROOT = args.product_root.resolve()
    git_evidence.cache_clear()
    try:
        workspace = args.workspace_root.resolve()
        receipt = args.receipt.resolve()
        receipt.relative_to(workspace)
        fail(args.pr_number < 1 or "/" not in args.repository, "PR identity is invalid")
        validator = delivery_validator()
        preflight(
            json.loads(receipt.read_text()), receipt, workspace,
            validator, args.product_root.resolve(),
        )
        lock_path = receipt.with_name(receipt.name + ".lock")
        lock_path.touch(mode=0o600, exist_ok=True)
        with lock_path.open("r+") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            run = json.loads(receipt.read_text())
            preflight(run, receipt, workspace, validator, PRODUCT_ROOT)
            fail(run.get("profile") != "software", "receipt profile must be software")
            acceptance = run.get("human_gates", {}).get("acceptance", {})
            fail(
                acceptance.get("status") == "approved",
                "merge evidence must be bound before human acceptance is recorded",
            )
            fail(run.get("software_delivery") is not None, "receipt already has a software_delivery binding")

            pr = github(f"repos/{args.repository}/pulls/{args.pr_number}")
            head = pr.get("head") if isinstance(pr.get("head"), dict) else {}
            head_commit = head.get("sha")
            merge_commit = pr.get("merge_commit_sha")
            fail(pr.get("number") != args.pr_number or pr.get("state") != "closed" or not pr.get("merged_at"),
                 "GitHub pull request is not merged")
            fail(not isinstance(pr.get("html_url"), str) or not isinstance(head_commit, str)
                 or not isinstance(merge_commit, str), "GitHub pull request identity is incomplete")
            head_tree = str(git(workspace, "rev-parse", f"{head_commit}^{{tree}}")).strip()
            merge_tree = str(git(workspace, "rev-parse", f"{merge_commit}^{{tree}}")).strip()
            fail(head_tree != merge_tree, "merged tree differs from reviewed PR head")
            checks = github(
                f"repos/{args.repository}/commits/{merge_commit}/check-runs"
                "?check_name=ci-status&filter=latest&per_page=100"
            ).get("check_runs")
            fail(not isinstance(checks, list), "GitHub check-runs response is incomplete")
            matches = [
                item for item in checks if isinstance(item, dict)
                and item.get("name") == "ci-status" and item.get("head_sha") == merge_commit
                and item.get("status") == "completed" and item.get("conclusion") == "success"
                and isinstance(item.get("completed_at"), str)
            ]
            fail(len(matches) != 1, "exact merged commit lacks one successful completed ci-status check")
            check = matches[0]

            ids = {item.get("id") for item in run.get("artifacts", []) if isinstance(item, dict)}
            fail(bool(ids & {"merged-source", "github-pr", "github-ci"}),
                 "receipt already uses a reserved software-delivery artifact id")
            relative_run_dir = receipt.parent.relative_to(workspace)
            evidence_dir = relative_run_dir / "github"
            payloads: list[tuple[str, bytes]] = [
                ("github-pr", encode({
                    "schema_version": 1, "contract": "github-pull-request-evidence",
                    "repository": args.repository, "number": args.pr_number, "url": pr["html_url"],
                    "head_commit": head_commit, "merge_commit": merge_commit, "state": "merged",
                })),
                ("github-ci", encode({
                    "schema_version": 1, "contract": "github-ci-evidence",
                    "repository": args.repository, "commit": merge_commit,
                    "check": "ci-status", "conclusion": "success", "completed_at": check["completed_at"],
                })),
            ]
            review_ids: list[str] = []
            for index, source in enumerate(args.review_artifact, start=1):
                raw = source.resolve().read_bytes()
                review = json.loads(raw)
                fail(not isinstance(review, dict) or review.get("contract") != "code-review-evidence"
                     or review.get("schema_version") != 1 or review.get("repository") != args.repository
                     or review.get("commit") != head_commit or review.get("verdict") != "pass",
                     f"review artifact {index} is not a passing exact-head review")
                artifact_id = f"github-review-{index}"
                fail(artifact_id in ids, f"receipt already uses {artifact_id}")
                review_ids.append(artifact_id)
                payloads.append((artifact_id, raw))

            additions = [{
                "id": "merged-source",
                "git_revision": {"repository": ".", "commit": merge_commit, "tree": merge_tree},
                "media_type": "application/x-git-revision", "artifact_type": "source",
                "class": "canonical", "owner": "delivery-chair", "retention": "project-policy",
            }]
            for artifact_id, raw in payloads:
                additions.append(artifact((evidence_dir / f"{artifact_id}.json").as_posix(), artifact_id, raw))
            run["artifacts"].extend(additions)
            run["security"]["artifact_surfaces"].append({"artifact_id": "merged-source", "surfaces": ["source"]})
            run["software_delivery"] = {
                "schema_version": 1, "contract": "software-delivery-binding",
                "canonical_artifact_id": "merged-source", "pull_request_artifact_id": "github-pr",
                "ci_artifact_id": "github-ci", "review_artifact_ids": review_ids,
            }
            run["checkpoint"].update({
                "generation": run["checkpoint"]["generation"] + 1,
                "current_slice": "awaiting-acceptance",
                "next_action": "request human acceptance of the exact merged artifact",
            })

            target_dir = workspace / evidence_dir
            target_dir.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(prefix=".software-bind-", dir=receipt.parent) as temporary:
                stage = Path(temporary)
                for artifact_id, raw in payloads:
                    target = target_dir / f"{artifact_id}.json"
                    fail(target.exists() and target.read_bytes() != raw,
                         f"refusing to replace conflicting evidence artifact: {target}")
                    staged = stage / f"{artifact_id}.json"
                    staged.write_bytes(raw)
                    with staged.open("rb") as handle:
                        os.fsync(handle.fileno())
                staged_receipt = stage / "RUN.json"
                staged_receipt.write_text(json.dumps(run, indent=2) + "\n")
                with staged_receipt.open("rb") as handle:
                    os.fsync(handle.fileno())
                for artifact_id, _ in payloads:
                    target = target_dir / f"{artifact_id}.json"
                    if not target.exists():
                        os.replace(stage / f"{artifact_id}.json", target)
                fsync_directory(target_dir)
                preflight(run, receipt, workspace, validator, PRODUCT_ROOT)
                os.replace(staged_receipt, receipt)
                fsync_directory(receipt.parent)
        print(f"PASS: bound merged software artifact {merge_commit} to {receipt}")
    except (OSError, ValueError, json.JSONDecodeError, subprocess.CalledProcessError) as exc:
        print(f"FAIL: {exc}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
