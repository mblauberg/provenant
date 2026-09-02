#!/usr/bin/env python3
"""Validate the post-merge software binding inside a delivery-run receipt."""

from __future__ import annotations

from datetime import datetime
from functools import lru_cache
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
from typing import Any


OID = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
SHA256 = re.compile(r"^sha256:[0-9a-f]{64}$")
# `scripts/lib/roots.py` is the single resolver for the product root (#754).
# The fallback loads that one file when this script is run directly by path and
# the product root is not on `sys.path`: it locates the resolver, it does not
# decide the root, and it leaves import resolution untouched (#755).
try:
    from scripts.lib.roots import product_root
except ModuleNotFoundError:  # pragma: no cover - direct invocation by path
    import importlib.util as _roots_util
    _roots_spec = _roots_util.spec_from_file_location(
        "provenant_roots", Path(__file__).resolve().parents[3] / "scripts" / "lib" / "roots.py"
    )
    _roots_module = _roots_util.module_from_spec(_roots_spec)
    _roots_spec.loader.exec_module(_roots_module)
    product_root = _roots_module.product_root

ROOT = product_root()
GIT_ARTIFACT_FIELDS = {
    "id", "git_revision", "media_type", "artifact_type", "class", "owner", "retention",
}


@lru_cache(maxsize=1)
def _git_evidence():
    path = ROOT / "scripts/git_evidence.py"
    spec = importlib.util.spec_from_file_location("delivery_git_evidence", path)
    if not spec or not spec.loader:
        raise RuntimeError("canonical Git evidence runner is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def configure_product_root(root: Path) -> None:
    global ROOT
    ROOT = root
    _git_evidence.cache_clear()


def _fail(condition: bool, message: str, invalid_type: type[ValueError]) -> None:
    if condition:
        raise invalid_type(message)


def _timestamp(value: Any) -> bool:
    if not isinstance(value, str) or not value.endswith("Z"):
        return False
    try:
        datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError:
        return False
    return True


def _load_artifact(
    artifact: dict[str, Any], *, artifact_root: Path | None,
    invalid_type: type[ValueError], field: str,
) -> dict[str, Any]:
    locations = {key for key in ("path", "uri", "git_revision") if key in artifact}
    _fail(locations != {"path"} or not isinstance(artifact.get("path"), str) or not artifact["path"],
          f"{field} must be a local artifact", invalid_type)
    _fail(artifact.get("media_type") != "application/json" or artifact.get("class") != "evidence",
          f"{field} must be a local JSON evidence artifact", invalid_type)
    _fail(artifact_root is None, f"{field} validation requires an artifact root", invalid_type)
    def no_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        value: dict[str, Any] = {}
        for key, item in pairs:
            _fail(key in value, f"{field} contains duplicate JSON key: {key}", invalid_type)
            value[key] = item
        return value

    try:
        value = json.loads((artifact_root / artifact["path"]).read_text(), object_pairs_hook=no_duplicates)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise invalid_type(f"{field} must contain readable JSON") from exc
    _fail(not isinstance(value, dict), f"{field} must contain a JSON object", invalid_type)
    return value


def _closed(value: dict[str, Any], fields: set[str], field: str, invalid_type: type[ValueError]) -> None:
    _fail(set(value) != fields, f"{field} fields are invalid", invalid_type)


def _native_object_format(repository_root: Path, invalid_type: type[ValueError]) -> tuple[str, int]:
    try:
        object_format = str(_git_evidence().git_output(
            repository_root, "rev-parse", "--show-object-format",
        )).strip()
    except (OSError, subprocess.CalledProcessError) as exc:
        raise invalid_type("Git repository object format is unavailable") from exc
    sizes = {"sha1": 40, "sha256": 64}
    _fail(object_format not in sizes, "Git repository object format is unsupported", invalid_type)
    return object_format, sizes[object_format]


def _resolve_exact_commit_tree(
    repository_root: Path, commit: Any, *, invalid_type: type[ValueError], field: str,
) -> tuple[str, int]:
    _, oid_size = _native_object_format(repository_root, invalid_type)
    _fail(not isinstance(commit, str) or not re.fullmatch(f"[0-9a-f]{{{oid_size}}}", commit),
          f"{field} does not match the repository native object format", invalid_type)
    try:
        object_type = str(_git_evidence().git_output(repository_root, "cat-file", "-t", commit)).strip()
    except (OSError, subprocess.CalledProcessError) as exc:
        raise invalid_type(f"{field} cannot resolve the committed artifact") from exc
    _fail(object_type != "commit", f"{field} must identify an exact commit object", invalid_type)
    try:
        resolved_commit = str(_git_evidence().git_output(
            repository_root, "rev-parse", "--verify", f"{commit}^{{commit}}",
        )).strip()
        tree = str(_git_evidence().git_output(
            repository_root, "rev-parse", "--verify", f"{commit}^{{tree}}",
        )).strip()
        tree_type = str(_git_evidence().git_output(repository_root, "cat-file", "-t", tree)).strip()
    except (OSError, subprocess.CalledProcessError) as exc:
        raise invalid_type(f"{field} cannot resolve the committed artifact") from exc
    _fail(resolved_commit != commit,
          f"{field} must identify an exact commit object", invalid_type)
    _fail(not re.fullmatch(f"[0-9a-f]{{{oid_size}}}", tree) or tree_type != "tree",
          f"{field} resolved tree is unavailable or invalid", invalid_type)
    return tree, oid_size


def validate_git_revision(
    revision: Any, *, artifact_id: str, media_type: Any, digest: Any,
    digest_present: bool, unavailable_present: bool,
    workspace_root: Path | None, allowed_source_paths: list[str], verify_hashes: bool,
    safe_path: Any, inside: Any, invalid_type: type[ValueError],
) -> None:
    _fail(not isinstance(revision, dict), f"artifact {artifact_id}.git_revision must be an object", invalid_type)
    _closed(revision, {"repository", "commit", "tree"}, f"artifact {artifact_id}.git_revision", invalid_type)
    repository = safe_path(revision.get("repository"), f"artifact {artifact_id}.git_revision.repository")
    _fail(not any(inside(repository, scope) for scope in allowed_source_paths),
          f"artifact {artifact_id}.git_revision repository is outside authority.allowed_source_paths", invalid_type)
    commit = revision.get("commit")
    tree = revision.get("tree")
    _fail(not isinstance(commit, str) or not OID.fullmatch(commit),
          f"artifact {artifact_id}.git_revision.commit is invalid", invalid_type)
    _fail(not isinstance(tree, str) or not OID.fullmatch(tree),
          f"artifact {artifact_id}.git_revision.tree is invalid", invalid_type)
    _fail(len(commit) != len(tree),
          f"artifact {artifact_id}.git_revision commit and tree object widths differ", invalid_type)
    legacy = media_type == "application/x-git-archive"
    _fail(media_type not in {"application/x-git-revision", "application/x-git-archive"},
          f"artifact {artifact_id}.git_revision media_type is invalid", invalid_type)
    if legacy:
        _fail(not digest_present or unavailable_present or not isinstance(digest, str) or not SHA256.fullmatch(digest),
              f"artifact {artifact_id}.git_revision legacy archive requires one SHA-256 digest", invalid_type)
    else:
        _fail(digest_present or unavailable_present,
              f"artifact {artifact_id}.git_revision must use commit and tree without digest fields", invalid_type)
    if not verify_hashes:
        return
    _fail(workspace_root is None, "verify_hashes requires workspace_root", invalid_type)
    repository_root = (workspace_root / repository).resolve()
    try:
        repository_root.relative_to(workspace_root.resolve())
    except ValueError as exc:
        raise invalid_type(f"artifact {artifact_id}.git_revision repository resolves outside workspace_root") from exc
    _fail(not (repository_root / ".git").exists(),
          f"artifact {artifact_id}.git_revision repository is not a Git worktree", invalid_type)
    live_tree, oid_size = _resolve_exact_commit_tree(
        repository_root, commit, invalid_type=invalid_type,
        field=f"artifact {artifact_id}.git_revision",
    )
    _fail(not re.fullmatch(f"[0-9a-f]{{{oid_size}}}", tree),
          f"artifact {artifact_id}.git_revision.tree does not match the repository native object format", invalid_type)
    _fail(live_tree != tree, f"artifact {artifact_id}.git_revision tree does not match commit", invalid_type)
    if legacy:
        try:
            archive = _git_evidence().git_output(
                repository_root, "archive", "--format=tar", commit, text=False,
            )
        except (OSError, subprocess.CalledProcessError) as exc:
            raise invalid_type(f"artifact {artifact_id}.git_revision cannot read the legacy archive") from exc
        actual = "sha256:" + hashlib.sha256(archive).hexdigest()
        _fail(actual != digest, f"artifact {artifact_id} digest does not match the committed Git archive", invalid_type)


def validate_git_artifact(
    item: dict[str, Any], artifact_id: str, path: Any, uri: Any,
    workspace_root: Path | None, allowed_source_paths: list[str], verify_hashes: bool,
    safe_path: Any, inside: Any, invalid_type: type[ValueError],
) -> None:
    locations = {field for field in ("path", "uri", "git_revision") if field in item}
    _fail(len(locations) != 1,
          f"artifact {artifact_id} requires exactly one path, uri or git_revision", invalid_type)
    if "uri" in locations:
        _fail(not isinstance(uri, str) or not uri.strip(),
              f"artifact {artifact_id}.uri must be a non-empty string", invalid_type)
    if "git_revision" in locations:
        revision = item["git_revision"]
        media_type = item.get("media_type")
        fields = GIT_ARTIFACT_FIELDS | ({"digest"} if media_type == "application/x-git-archive" else set())
        validate_git_revision(
            revision, artifact_id=artifact_id, media_type=media_type, digest=item.get("digest"),
            digest_present="digest" in item,
            unavailable_present="digest_unavailable_reason" in item, workspace_root=workspace_root,
            allowed_source_paths=allowed_source_paths, verify_hashes=verify_hashes,
            safe_path=safe_path, inside=inside, invalid_type=invalid_type,
        )
        _closed(item, fields, f"artifact {artifact_id}", invalid_type)


def validate_integrity_shape(
    item: dict[str, Any], artifact_id: str, revision_present: bool, path_present: bool,
    digest_validator: Any, fail: Any,
) -> None:
    digest_present = "digest" in item
    unavailable_present = "digest_unavailable_reason" in item
    fail(not revision_present and digest_present == unavailable_present,
         f"artifact {artifact_id} requires digest xor digest_unavailable_reason")
    if digest_present:
        digest_validator(item["digest"], f"artifact {artifact_id}.digest")
    if unavailable_present:
        unavailable = item["digest_unavailable_reason"]
        fail(not isinstance(unavailable, str) or not unavailable.strip(),
             f"artifact {artifact_id}.digest_unavailable_reason must be non-empty text")
    if path_present:
        fail(not revision_present and (not digest_present or unavailable_present),
             f"local artifact {artifact_id} requires digest")


def validate_if_software(
    run: dict[str, Any], artifacts: dict[str, dict[str, Any]],
    artifact_root: Path | None, verify_hashes: bool,
    invalid_type: type[ValueError],
) -> None:
    if run.get("profile") == "software":
        validate(
            run, artifacts, artifact_root=artifact_root,
            verify_hashes=verify_hashes, invalid_type=invalid_type,
        )


def validate(
    run: dict[str, Any], artifacts: dict[str, dict[str, Any]], *,
    artifact_root: Path | None, verify_hashes: bool,
    invalid_type: type[ValueError],
) -> None:
    """Require a local exact-merge evidence chain before software acceptance."""
    binding = run.get("software_delivery")
    if binding is None:
        return
    _fail(not isinstance(binding, dict), "software_delivery must be an object", invalid_type)
    fields = {
        "schema_version", "contract", "canonical_artifact_id",
        "pull_request_artifact_id", "ci_artifact_id", "review_artifact_ids",
    }
    _closed(binding, fields, "software_delivery", invalid_type)
    _fail(binding.get("schema_version") != 1 or binding.get("contract") != "software-delivery-binding",
          "software_delivery contract is invalid", invalid_type)
    canonical = artifacts.get(binding.get("canonical_artifact_id"))
    _fail(not canonical or canonical.get("class") != "canonical" or not canonical.get("git_revision"),
          "software_delivery canonical artifact must be a Git revision", invalid_type)
    revision = canonical["git_revision"]
    merge_commit = revision.get("commit")

    pr_artifact = artifacts.get(binding.get("pull_request_artifact_id"))
    ci_artifact = artifacts.get(binding.get("ci_artifact_id"))
    _fail(not pr_artifact, "software_delivery pull request artifact is missing", invalid_type)
    _fail(not ci_artifact, "software_delivery CI artifact is missing", invalid_type)
    review_ids = binding.get("review_artifact_ids")
    _fail(not isinstance(review_ids, list) or not review_ids or len(set(review_ids)) != len(review_ids),
          "software_delivery review_artifact_ids must be non-empty and unique", invalid_type)
    for index, artifact_id in enumerate(review_ids):
        _fail(artifact_id not in artifacts, f"software_delivery review artifact {index} is missing", invalid_type)
    if not verify_hashes:
        return

    pr = _load_artifact(pr_artifact, artifact_root=artifact_root, invalid_type=invalid_type,
                        field="software_delivery pull request artifact")
    _closed(pr, {"schema_version", "contract", "repository", "number", "url", "head_commit", "merge_commit", "state"},
            "software_delivery pull request artifact", invalid_type)
    _fail(pr.get("schema_version") != 1 or pr.get("contract") != "github-pull-request-evidence",
          "software_delivery pull request artifact contract is invalid", invalid_type)
    _fail(pr.get("state") != "merged" or not isinstance(pr.get("number"), int) or pr["number"] < 1,
          "software_delivery pull request artifact must record a merged PR", invalid_type)
    _fail(not isinstance(pr.get("repository"), str) or not pr["repository"] or not isinstance(pr.get("url"), str) or not pr["url"],
          "software_delivery pull request artifact identity is invalid", invalid_type)
    _fail(not OID.fullmatch(str(pr.get("head_commit", ""))) or pr.get("merge_commit") != merge_commit,
          "software_delivery pull request artifact does not bind the merged revision", invalid_type)

    ci = _load_artifact(ci_artifact, artifact_root=artifact_root, invalid_type=invalid_type,
                        field="software_delivery CI artifact")
    _closed(ci, {"schema_version", "contract", "repository", "commit", "check", "conclusion", "completed_at"},
            "software_delivery CI artifact", invalid_type)
    _fail(ci.get("schema_version") != 1 or ci.get("contract") != "github-ci-evidence",
          "software_delivery CI artifact contract is invalid", invalid_type)
    _fail(ci.get("repository") != pr.get("repository") or ci.get("commit") != merge_commit
          or ci.get("check") != "ci-status" or ci.get("conclusion") != "success"
          or not _timestamp(ci.get("completed_at")),
          "software_delivery CI artifact must bind successful ci-status on the merged revision", invalid_type)

    passing_reviews = [item for item in run.get("reviews", []) if isinstance(item, dict) and item.get("status") == "pass"]
    matched: set[tuple[str, str, str]] = set()
    for index, artifact_id in enumerate(review_ids):
        artifact = artifacts.get(artifact_id)
        review = _load_artifact(artifact, artifact_root=artifact_root, invalid_type=invalid_type,
                                field=f"software_delivery review artifact {index}")
        _closed(review, {"schema_version", "contract", "repository", "commit", "provider_family", "adapter", "model", "verdict", "reviewed_at"},
                f"software_delivery review artifact {index}", invalid_type)
        _fail(review.get("schema_version") != 1 or review.get("contract") != "code-review-evidence",
              f"software_delivery review artifact {index} contract is invalid", invalid_type)
        lineage = (review.get("provider_family"), review.get("adapter"), review.get("model"))
        _fail(review.get("repository") != pr.get("repository") or review.get("commit") != pr.get("head_commit")
              or review.get("verdict") != "pass" or not _timestamp(review.get("reviewed_at")),
              f"software_delivery review artifact {index} does not bind the reviewed PR head", invalid_type)
        _fail(not any(
            (item.get("provider_family"), item.get("adapter"), item.get("model")) == lineage
            for item in passing_reviews
        ), f"software_delivery review artifact {index} does not match a passing receipt review", invalid_type)
        matched.add(lineage)
    required_primary = {
        (item.get("provider_family"), item.get("adapter"), item.get("model"))
        for item in passing_reviews
        if item.get("role") in {"targeted", "other-primary", "distinct-family"}
    }
    _fail(not required_primary <= matched,
          "software_delivery review artifacts must retain every passing primary review", invalid_type)

    if artifact_root is not None:
        repository = (artifact_root / revision["repository"]).resolve()
        merge_tree, _ = _resolve_exact_commit_tree(
            repository, merge_commit, invalid_type=invalid_type,
            field="software_delivery merged Git revision",
        )
        head_tree, _ = _resolve_exact_commit_tree(
            repository, pr["head_commit"], invalid_type=invalid_type,
            field="software_delivery reviewed Git revision",
        )
        _fail(merge_tree != head_tree,
              "software_delivery merged tree differs from the reviewed PR head", invalid_type)
