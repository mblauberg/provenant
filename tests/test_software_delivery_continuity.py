import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import os

import pytest


ROOT = Path(__file__).resolve().parents[1]


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


DELIVERY = load(ROOT / "skills/deliver/scripts/validate_delivery.py", "continuity_delivery")
REFERENCE = load(ROOT / "skills/deliver/scripts/reference_runs.py", "continuity_reference")
MATERIALISE = load(ROOT / "skills/deliver/scripts/reference_evaluation.py", "continuity_materialise")


def git(root: Path, *args: str, text: bool = True):
    return subprocess.run(
        ["git", "-C", str(root), *args], check=True, capture_output=True, text=text,
    ).stdout


def write_json_artifact(workspace: Path, artifact_id: str, value: dict) -> dict:
    path = Path("evidence") / f"{artifact_id}.json"
    target = workspace / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(value, sort_keys=True) + "\n")
    return {
        "id": artifact_id,
        "path": path.as_posix(),
        "media_type": "application/json",
        "artifact_type": "evidence",
        "digest": "sha256:" + hashlib.sha256(target.read_bytes()).hexdigest(),
        "class": "evidence",
        "owner": "delivery-chair",
        "retention": "risk-policy",
    }


def merged_software_delivery(workspace: Path) -> dict:
    workspace.mkdir(parents=True, exist_ok=True)
    git(workspace, "init")
    git(workspace, "config", "user.email", "fixture@example.test")
    git(workspace, "config", "user.name", "Fixture")
    (workspace / "product.txt").write_text("reviewed product\n")
    git(workspace, "add", "product.txt")
    git(workspace, "commit", "-m", "reviewed head")
    head = git(workspace, "rev-parse", "HEAD").strip()
    git(workspace, "commit", "--allow-empty", "-m", "merge pull request")
    merged = git(workspace, "rev-parse", "HEAD").strip()
    tree = git(workspace, "rev-parse", "HEAD^{tree}").strip()
    run = REFERENCE.make_reference_run("software", ROOT)
    MATERIALISE.materialise_reference_run(run, workspace, ROOT)
    run["authority"]["allowed_source_paths"] = ["."]
    run["artifacts"].append({
        "id": "merged-source",
        "git_revision": {"repository": ".", "commit": merged, "tree": tree},
        "media_type": "application/x-git-revision",
        "artifact_type": "source",
        "class": "canonical",
        "owner": "delivery-chair",
        "retention": "project-policy",
    })
    run["security"]["artifact_surfaces"].append({
        "artifact_id": "merged-source", "surfaces": ["source"],
    })
    repository = "example/project"
    run["artifacts"].extend([
        write_json_artifact(workspace, "github-pr", {
            "schema_version": 1, "contract": "github-pull-request-evidence",
            "repository": repository, "number": 42, "url": "https://example.test/pr/42",
            "head_commit": head, "merge_commit": merged, "state": "merged",
        }),
        write_json_artifact(workspace, "github-ci", {
            "schema_version": 1, "contract": "github-ci-evidence",
            "repository": repository, "commit": merged, "check": "ci-status",
            "conclusion": "success", "completed_at": "2026-07-10T00:08:30Z",
        }),
        write_json_artifact(workspace, "review-openai", {
            "schema_version": 1, "contract": "code-review-evidence",
            "repository": repository, "commit": head, "provider_family": "openai",
            "adapter": "native-subagent", "model": "runtime-resolved", "verdict": "pass",
            "reviewed_at": "2026-07-10T00:07:30Z",
        }),
        write_json_artifact(workspace, "review-anthropic", {
            "schema_version": 1, "contract": "code-review-evidence",
            "repository": repository, "commit": head, "provider_family": "anthropic",
            "adapter": "claude-code", "model": "runtime-resolved", "verdict": "pass",
            "reviewed_at": "2026-07-10T00:08:00Z",
        }),
    ])
    run["software_delivery"] = {
        "schema_version": 1,
        "contract": "software-delivery-binding",
        "canonical_artifact_id": "merged-source",
        "pull_request_artifact_id": "github-pr",
        "ci_artifact_id": "github-ci",
        "review_artifact_ids": ["review-openai", "review-anthropic"],
    }
    return run


def source_only_delivery(workspace: Path) -> tuple[dict, dict]:
    run = merged_software_delivery(workspace)
    del run["software_delivery"]
    artifact = next(item for item in run["artifacts"] if item["id"] == "merged-source")
    return run, artifact


def convert_source_to_legacy_archive(run: dict, workspace: Path) -> dict:
    artifact = next(item for item in run["artifacts"] if item["id"] == "merged-source")
    commit = artifact["git_revision"]["commit"]
    archive = git(workspace, "archive", "--format=tar", commit, text=False)
    artifact["media_type"] = "application/x-git-archive"
    artifact["digest"] = "sha256:" + hashlib.sha256(archive).hexdigest()
    return artifact


def before_acceptance(run: dict) -> None:
    """The binder runs before human acceptance is recorded."""
    run["human_gates"]["acceptance"] = {"status": "pending", "approver": "", "evidence": ""}


def test_merged_software_binding_rejects_reviewed_tree_drift(tmp_path):
    run = merged_software_delivery(tmp_path)
    DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)

    pr_path = tmp_path / "evidence/github-pr.json"
    pr = json.loads(pr_path.read_text())
    pr["head_commit"] = run["software_delivery"]["canonical_artifact_id"]
    pr_path.write_text(json.dumps(pr) + "\n")
    artifact = next(item for item in run["artifacts"] if item["id"] == "github-pr")
    artifact["digest"] = "sha256:" + hashlib.sha256(pr_path.read_bytes()).hexdigest()
    with pytest.raises(DELIVERY.Invalid, match="does not bind the merged revision"):
        DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)


def test_git_revision_uses_commit_and_resolved_tree_without_redundant_archive_digest(tmp_path):
    run = merged_software_delivery(tmp_path)

    DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)


def test_frozen_legacy_git_archive_receipt_remains_hash_verified(tmp_path):
    run = merged_software_delivery(tmp_path)
    artifact = convert_source_to_legacy_archive(run, tmp_path)
    DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)

    artifact["digest"] = "sha256:" + "0" * 64
    with pytest.raises(DELIVERY.Invalid, match="digest does not match the committed Git archive"):
        DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)


def test_git_revision_rejects_digest_fields_and_commit_tree_mismatch(tmp_path):
    run = merged_software_delivery(tmp_path)
    artifact = next(item for item in run["artifacts"] if item["id"] == "merged-source")
    artifact["digest"] = "sha256:" + "0" * 64
    with pytest.raises(DELIVERY.Invalid, match="must use commit and tree without digest fields"):
        DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)

    artifact.pop("digest")
    artifact["git_revision"]["tree"] = "0" * 40
    with pytest.raises(DELIVERY.Invalid, match="tree does not match commit"):
        DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)


def test_git_revision_cannot_also_claim_a_path(tmp_path):
    run = merged_software_delivery(tmp_path)
    artifact = next(item for item in run["artifacts"] if item["id"] == "merged-source")
    artifact["path"] = "product.txt"

    with pytest.raises(DELIVERY.Invalid, match="requires exactly one path, uri or git_revision"):
        DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)


def test_git_revision_rejects_an_unavailable_tree_object(tmp_path):
    run = merged_software_delivery(tmp_path)
    artifact = next(item for item in run["artifacts"] if item["id"] == "merged-source")
    tree = artifact["git_revision"]["tree"]
    (tmp_path / ".git" / "objects" / tree[:2] / tree[2:]).unlink()

    with pytest.raises(DELIVERY.Invalid, match="cannot resolve the committed artifact"):
        DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)


def test_git_revision_does_not_lazy_fetch_a_missing_promisor_tree(tmp_path):
    origin = tmp_path / "origin"
    origin.mkdir()
    git(origin, "init")
    git(origin, "config", "user.email", "fixture@example.test")
    git(origin, "config", "user.name", "Fixture")
    (origin / "product.txt").write_text("promisor product\n")
    git(origin, "add", "product.txt")
    git(origin, "commit", "-m", "promisor source")
    git(origin, "config", "uploadpack.allowFilter", "true")
    commit = git(origin, "rev-parse", "HEAD").strip()
    tree = git(origin, "rev-parse", "HEAD^{tree}").strip()
    workspace = tmp_path / "workspace"
    result = subprocess.run([
        "git", "clone", "--filter=tree:0", "--no-checkout",
        origin.as_uri(), str(workspace),
    ], capture_output=True, text=True)
    if result.returncode != 0:
        pytest.skip(f"installed Git does not support local partial clone: {result.stderr}")
    run = REFERENCE.make_reference_run("software", ROOT)
    MATERIALISE.materialise_reference_run(run, workspace, ROOT)
    run["authority"]["allowed_source_paths"] = ["."]
    run["artifacts"].append({
        "id": "promisor-source",
        "git_revision": {"repository": ".", "commit": commit, "tree": tree},
        "media_type": "application/x-git-revision",
        "artifact_type": "source", "class": "canonical",
        "owner": "delivery-chair", "retention": "project-policy",
    })
    run["security"]["artifact_surfaces"].append({
        "artifact_id": "promisor-source", "surfaces": ["source"],
    })

    with pytest.raises(DELIVERY.Invalid, match="cannot resolve the committed artifact"):
        DELIVERY.validate(run, ROOT, workspace_root=workspace, verify_hashes=True)
    unavailable = subprocess.run(
        ["git", "-C", str(workspace), "cat-file", "-e", tree],
        env={**os.environ, "GIT_NO_LAZY_FETCH": "1"}, capture_output=True,
    )
    assert unavailable.returncode != 0


@pytest.mark.parametrize("variable, suffix", [
    ("GIT_DIR", ".git"),
    ("GIT_OBJECT_DIRECTORY", ".git/objects"),
])
def test_git_revision_ignores_inherited_repository_and_object_redirects(
    tmp_path, monkeypatch, variable, suffix,
):
    workspace = tmp_path / "workspace"
    run, artifact = source_only_delivery(workspace)
    external = tmp_path / "external"
    _, external_artifact = source_only_delivery(external)
    (external / "product.txt").write_text("external-only product\n")
    git(external, "add", "product.txt")
    git(external, "commit", "-m", "external-only commit")
    artifact["git_revision"] = {
        **external_artifact["git_revision"],
        "commit": git(external, "rev-parse", "HEAD").strip(),
        "tree": git(external, "rev-parse", "HEAD^{tree}").strip(),
    }
    monkeypatch.setenv(variable, str(external / suffix))

    with pytest.raises(DELIVERY.Invalid, match="cannot resolve the committed artifact"):
        DELIVERY.validate(run, ROOT, workspace_root=workspace, verify_hashes=True)


def test_git_revision_ignores_replace_objects_when_resolving_the_tree(tmp_path):
    run, artifact = source_only_delivery(tmp_path)
    original_commit = artifact["git_revision"]["commit"]
    (tmp_path / "product.txt").write_text("replacement product\n")
    git(tmp_path, "add", "product.txt")
    git(tmp_path, "commit", "-m", "replacement commit")
    replacement_commit = git(tmp_path, "rev-parse", "HEAD").strip()
    replacement_tree = git(tmp_path, "rev-parse", "HEAD^{tree}").strip()
    git(tmp_path, "replace", original_commit, replacement_commit)
    artifact["git_revision"]["tree"] = replacement_tree

    with pytest.raises(DELIVERY.Invalid, match="tree does not match commit"):
        DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)


@pytest.mark.parametrize("object_kind", ["tree", "tag"])
def test_git_revision_requires_the_declared_object_itself_to_be_a_commit(tmp_path, object_kind):
    run, artifact = source_only_delivery(tmp_path)
    commit = artifact["git_revision"]["commit"]
    if object_kind == "tree":
        artifact["git_revision"]["commit"] = artifact["git_revision"]["tree"]
    else:
        git(tmp_path, "tag", "-a", "release-candidate", "-m", "candidate", commit)
        artifact["git_revision"]["commit"] = git(
            tmp_path, "rev-parse", "release-candidate^{tag}",
        ).strip()

    with pytest.raises(DELIVERY.Invalid, match="must identify an exact commit object"):
        DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)


def test_sha256_git_revision_requires_full_native_commit_and_tree_widths(tmp_path):
    workspace = tmp_path / "sha256"
    workspace.mkdir()
    result = subprocess.run(
        ["git", "-C", str(workspace), "init", "--object-format=sha256"],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        pytest.skip("installed Git does not support SHA-256 repositories")
    git(workspace, "config", "user.email", "fixture@example.test")
    git(workspace, "config", "user.name", "Fixture")
    (workspace / "product.txt").write_text("sha256 product\n")
    git(workspace, "add", "product.txt")
    git(workspace, "commit", "-m", "sha256 source")
    commit = git(workspace, "rev-parse", "HEAD").strip()
    tree = git(workspace, "rev-parse", "HEAD^{tree}").strip()
    run = REFERENCE.make_reference_run("software", ROOT)
    MATERIALISE.materialise_reference_run(run, workspace, ROOT)
    run["authority"]["allowed_source_paths"] = ["."]
    run["artifacts"].append({
        "id": "sha256-source",
        "git_revision": {"repository": ".", "commit": commit, "tree": tree},
        "media_type": "application/x-git-revision",
        "artifact_type": "source",
        "class": "canonical",
        "owner": "delivery-chair",
        "retention": "project-policy",
    })
    run["security"]["artifact_surfaces"].append({
        "artifact_id": "sha256-source", "surfaces": ["source"],
    })

    DELIVERY.validate(run, ROOT, workspace_root=workspace, verify_hashes=True)

    artifact = next(item for item in run["artifacts"] if item["id"] == "sha256-source")
    artifact["git_revision"]["tree"] = tree[:40]
    with pytest.raises(DELIVERY.Invalid, match="object widths differ"):
        DELIVERY.validate(run, ROOT, workspace_root=workspace, verify_hashes=True)

    artifact["git_revision"].update({"commit": commit[:40], "tree": tree})
    with pytest.raises(DELIVERY.Invalid, match="object widths differ"):
        DELIVERY.validate(run, ROOT, workspace_root=workspace, verify_hashes=True)


def test_non_git_local_artifacts_still_require_and_verify_sha256(tmp_path):
    run = merged_software_delivery(tmp_path)
    artifact = next(item for item in run["artifacts"] if item["id"] == "github-pr")
    digest = artifact.pop("digest")
    with pytest.raises(DELIVERY.Invalid, match="requires digest xor digest_unavailable_reason"):
        DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)

    artifact["digest"] = digest
    (tmp_path / artifact["path"]).write_text("{}\n")
    with pytest.raises(DELIVERY.Invalid, match="digest does not match live bytes"):
        DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)


@pytest.mark.parametrize("extra", [None, False, "", [], {}, {"reason": "offline"}])
def test_non_git_digest_union_uses_field_presence(tmp_path, extra):
    run, _ = source_only_delivery(tmp_path)
    artifact = next(item for item in run["artifacts"] if item["id"] == "github-pr")
    artifact["digest_unavailable_reason"] = extra

    with pytest.raises(DELIVERY.Invalid, match="requires digest xor digest_unavailable_reason"):
        DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)


@pytest.mark.parametrize("extra", [None, False, "", [], {}])
def test_artifact_location_union_uses_field_presence(tmp_path, extra):
    run, _ = source_only_delivery(tmp_path)
    artifact = next(item for item in run["artifacts"] if item["id"] == "github-pr")
    artifact["uri"] = extra

    with pytest.raises(DELIVERY.Invalid, match="requires exactly one path, uri or git_revision"):
        DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)


@pytest.mark.parametrize("reason", [None, False, "", [], {}, {"reason": "offline"}])
def test_external_unavailable_reason_requires_nonempty_text(tmp_path, reason):
    run, _ = source_only_delivery(tmp_path)
    artifact = next(item for item in run["artifacts"] if item["id"] == "github-pr")
    artifact.pop("path")
    artifact.pop("digest")
    artifact["uri"] = "https://example.test/evidence.json"
    artifact["digest_unavailable_reason"] = reason

    with pytest.raises(DELIVERY.Invalid, match="digest_unavailable_reason must be non-empty text"):
        DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)


@pytest.mark.parametrize("value", [None, False, "", [], {}])
def test_git_artifact_shape_rejects_extra_digest_fields_by_presence(tmp_path, value):
    run, artifact = source_only_delivery(tmp_path)
    artifact["digest"] = value

    with pytest.raises(DELIVERY.Invalid, match="must use commit and tree without digest fields"):
        DELIVERY.validate(run, ROOT, workspace_root=tmp_path, verify_hashes=True)
def test_binder_materialises_the_post_merge_chain_without_advancing_acceptance(tmp_path):
    run = merged_software_delivery(tmp_path)
    pr = json.loads((tmp_path / "evidence/github-pr.json").read_text())
    ci = json.loads((tmp_path / "evidence/github-ci.json").read_text())
    review_sources = []
    source_dir = tmp_path / "review-source"
    source_dir.mkdir()
    for name in ("review-openai", "review-anthropic"):
        source = source_dir / f"{name}.json"
        source.write_bytes((tmp_path / "evidence" / f"{name}.json").read_bytes())
        review_sources.append(source)
    generated = {"merged-source", "github-pr", "github-ci", "review-openai", "review-anthropic"}
    run["artifacts"] = [item for item in run["artifacts"] if item["id"] not in generated]
    run["security"]["artifact_surfaces"] = [
        item for item in run["security"]["artifact_surfaces"] if item["artifact_id"] != "merged-source"
    ]
    del run["software_delivery"]
    before_acceptance(run)
    for artifact_id in generated - {"merged-source"}:
        (tmp_path / "evidence" / f"{artifact_id}.json").unlink()
    receipt = tmp_path / "RUN.json"
    receipt.write_text(json.dumps(run))
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    gh = bin_dir / "gh"
    gh.write_text(
        "#!/bin/sh\n"
        "printf 'invoked\\n' >> \"$GH_MARKER\"\n"
        "case \"$*\" in\n"
        "  *check-runs*) printf '%s\\n' \"$GH_CHECKS_JSON\" ;;\n"
        "  *pulls*) printf '%s\\n' \"$GH_PR_JSON\" ;;\n"
        "  *) exit 9 ;;\n"
        "esac\n"
    )
    gh.chmod(0o755)
    marker = tmp_path / "gh-invocations"
    redirected_git_dir = tmp_path / "redirected.git"
    git(tmp_path, "init", "--bare", str(redirected_git_dir))
    environment = {
        **os.environ,
        "PATH": f"{bin_dir}:{os.environ['PATH']}",
        "GIT_DIR": str(redirected_git_dir),
        "GH_MARKER": str(marker),
        "GH_PR_JSON": json.dumps({
            "number": pr["number"], "state": "closed", "merged_at": "2026-07-10T00:08:00Z",
            "html_url": pr["url"], "head": {"sha": pr["head_commit"]},
            "merge_commit_sha": pr["merge_commit"],
        }),
        "GH_CHECKS_JSON": json.dumps({"check_runs": [{
            "name": "ci-status", "head_sha": pr["merge_commit"], "status": "completed",
            "conclusion": "success", "completed_at": ci["completed_at"],
        }]}),
    }
    command = [
        str(ROOT / "skills/implement/scripts/bind_merged_delivery.py"), str(receipt),
        "--workspace-root", str(tmp_path), "--repository", pr["repository"],
        "--pr-number", str(pr["number"]),
        *[argument for source in review_sources for argument in ("--review-artifact", str(source))],
    ]
    denied_receipt = receipt.read_bytes()
    denied = subprocess.run(command, env=environment, capture_output=True, text=True)
    assert denied.returncode == 1
    assert "allowlist api.github.com" in denied.stdout
    assert receipt.read_bytes() == denied_receipt
    assert not marker.exists()
    assert not (tmp_path / "github").exists()
    assert not receipt.with_name("RUN.json.lock").exists()

    run["authority"].update({
        "secrets_access": "use-without-disclosure",
        "secret_refs": ["github-cli-auth"],
        "network": {"tool_egress": "allowlist", "allowed_hosts": ["api.github.com"]},
    })
    receipt.write_text(json.dumps(run))
    first = subprocess.Popen(command, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    second = subprocess.Popen(command, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    results = [first.communicate(), second.communicate()]
    assert sorted((first.returncode, second.returncode)) == [0, 1], results
    bound = json.loads(receipt.read_text())
    assert bound["human_gates"]["acceptance"]["status"] == "pending"
    merged_source = next(item for item in bound["artifacts"] if item["id"] == "merged-source")
    assert merged_source["media_type"] == "application/x-git-revision"
    assert "digest" not in merged_source
    assert "digest_unavailable_reason" not in merged_source
    DELIVERY.validate(bound, ROOT, workspace_root=tmp_path, verify_hashes=True)
