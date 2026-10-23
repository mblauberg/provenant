"""CI/repository policy assurance tests.

Known residual (issue #179, following cross-family review of PR #168): the
`continue-on-error` guard below (`_assert_no_continue_on_error`) is an
in-repo pytest, which has an inherent self-referential limit that no amount
of test-side hardening can close. A PR that adds `continue-on-error: true`
to the very step that runs this suite (or to the `ci-status` aggregation
step itself) makes that step's failure invisible to the job's own
conclusion -- and `needs.<job>.result`, which `ci-status` reads to decide
whether to `exit 1`, reports exactly that conclusion. The assertion still
raises correctly inside the run; the required check simply never sees it.
The same PR could, just as easily, delete this test file outright. Neither
is fixable from inside the test suite: independent enforcement has to live
outside the PR's own workflow run.

What this repository already has (verified live via `gh api
repos/.../rulesets`, 2026-07-16): a branch ruleset on `main` pins `ci-status`
as a `required_status_checks` context with `strict_required_status_checks_
policy: true` and an empty `bypass_actors` list, so no actor can merge past
a red or pending `ci-status`. That closes the "merge without a green check"
gap; it does not close the "the green check lied" gap above.

What a personal/public, non-Enterprise repo does *not* get from rulesets:
rule types that restrict *edits* to specific paths (`file_path_restriction`,
`max_file_path_length`, `max_file_size`) are gated to GitHub Enterprise, so
this repo cannot use a ruleset to lock `.github/workflows/ci.yml`, the
composite action, or this test file against modification the way it locks
pushes to `main`. The only lever available at this plan tier is a *second*,
independently-sourced required status check -- one produced by a workflow
run the PR under review cannot itself edit (e.g. triggered via
`pull_request_target` against the base ref, or a scheduled post-merge
audit) -- so that a single continue-on-error edit inside the PR's own
`ci.yml` cannot neutralise both checks at once. That second-check work is
tracked in #196 (#170 covered `integration_id` pinning and main-push
concurrency, not an independently sourced check); nothing further is
actionable here as a test-suite change.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import tomllib
from pathlib import Path

import pytest
import yaml


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
SETUP_ACTION = ROOT / ".github" / "actions" / "setup-node-workspace" / "action.yml"
SETUP_ACTION_USES = "$/.github/actions/setup-node-workspace"
ROOT_PACKAGE = ROOT / "package.json"
ROOT_LOCK = ROOT / "package-lock.json"
FABRIC_PACKAGE = ROOT / "runtime" / "fabric" / "package.json"
IMMUTABLE_ACTION = re.compile(r"^[^@\s]+@[0-9a-f]{40}$")
# Local composite actions are pinned by the commit under review itself; only
# repository-local paths are exempt from the 40-hex SHA pin.
LOCAL_ACTION = re.compile(r"^\$/\.github/actions/[a-z0-9-]+$")
JOB_PERMISSIONS = {
    "detect-changes": {"actions": "read", "contents": "read", "pull-requests": "read"},
    "harness-checks": {"contents": "read"},
    "python-tests": {"contents": "read"},
    "lane-conformance": {"contents": "read"},
    "docs": {"contents": "read"},
    "fabric": {"contents": "read"},
    "split-root": {"contents": "read"},
    "rust": {"contents": "read"},
    "rust-macos-release": {"contents": "read"},
    "zizmor": {"contents": "read"},
    "ci-status": {},
}
FABRIC_GUIDE = ROOT / "runtime" / "fabric" / "README.md"


def _parse_workflow_text(text: str) -> dict[str, object]:
    value = yaml.safe_load(text)
    assert isinstance(value, dict)
    return value


def _workflow() -> dict[str, object]:
    return _parse_workflow_text(WORKFLOW.read_text(encoding="utf-8"))


def _job(document: dict[str, object], name: str) -> dict[str, object]:
    jobs = document.get("jobs")
    assert isinstance(jobs, dict)
    value = jobs.get(name)
    assert isinstance(value, dict), f"CI must define the {name} job"
    return value


def _steps(job: dict[str, object]) -> list[dict[str, object]]:
    value = job.get("steps")
    assert isinstance(value, list)
    assert all(isinstance(item, dict) for item in value)
    return value


def _assert_no_continue_on_error(
    document: dict[str, object],
    setup_steps: list[dict[str, object]] | None = None,
) -> None:
    # continue-on-error on any required job or step would neutralise failure
    # propagation invisibly, so no job and no step may set it. This also
    # covers the shared composite action (setup-node-workspace) that every
    # workspace job runs through via `uses: $/...` -- a continue-on-error
    # planted there is exactly as invisible to the ci-status aggregate as
    # one planted directly in ci.yml, and prior to this check it was outside
    # this guard's reach entirely, parsed and asserted on nowhere in this
    # suite. `setup_steps` defaults to the real, on-disk composite action
    # steps; tests pass a mutated copy to exercise the guard without
    # touching the file. No modelled exception exists today.
    jobs = document.get("jobs")
    assert isinstance(jobs, dict)
    for job_name, job in jobs.items():
        assert isinstance(job, dict)
        assert "continue-on-error" not in job, job_name
        for step in _steps(job):
            assert "continue-on-error" not in step, job_name
    for step in setup_steps if setup_steps is not None else _setup_action_steps():
        assert "continue-on-error" not in step, "setup-node-workspace"


def _triggers(document: dict[str, object]) -> dict[str, object]:
    # PyYAML resolves the bare `on:` key to boolean True (YAML 1.1), so read
    # the trigger mapping under whichever key the loader produced. This
    # accessor only extracts the mapping; it cannot verify the key was
    # actually spelled `on:` (every YAML-1.1 boolean spelling of "on"
    # collapses to the identical `True` key, so the parsed document can
    # never make that distinction) -- that is asserted separately, from the
    # raw text, by _assert_exact_on_key.
    on = document.get(True, document.get("on"))
    assert isinstance(on, dict)
    return on


# Every bare top-level key PyYAML's (YAML 1.1) bool resolver would fold to
# the same boolean tag as a correctly-spelled `on:` -- so none of these may
# appear verbatim, and `on` itself must appear exactly once.
_ON_KEY_LOOKALIKES = frozenset({"on", "off", "yes", "no", "true", "false"})
_TOP_LEVEL_KEY_PATTERN = re.compile(r"(?m)^([A-Za-z]+)\s*:")


def _assert_exact_on_key(text: str) -> None:
    # PyYAML (YAML 1.1) resolves every one of `on:`/`On:`/`ON:`/`true:`/...
    # to the identical boolean `True` mapping key, so the *parsed* document
    # can never tell a correctly-spelled bare `on:` trigger key apart from a
    # casing typo, or from a stray duplicate of it -- both parse identically.
    # GitHub's own workflow parser does not fold `on` to a boolean at all: it
    # requires the exact lowercase string "on" and silently ignores any
    # other spelling, so a typo parses cleanly through _triggers above yet
    # the workflow never fires on GitHub. Scan the raw text directly for
    # every top-level key that could have been intended as the trigger key --
    # including a duplicate arm added alongside the real one -- and require
    # there be exactly one, spelled exactly `on`.
    hits = [key for key in _TOP_LEVEL_KEY_PATTERN.findall(text) if key.lower() in _ON_KEY_LOOKALIKES]
    assert hits == ["on"], (
        "workflow must declare exactly one literal lowercase `on:` top-level "
        f"key with no YAML-1.1-boolean-spelled alias present; found {hits!r}"
    )


def _assert_trigger_semantics(document: dict[str, object]) -> None:
    _assert_exact_on_key(WORKFLOW.read_text(encoding="utf-8"))
    on = _triggers(document)
    assert set(on) == {"push", "pull_request", "merge_group"}
    # pull_request must fire on every PR: no paths, paths-ignore, branches or
    # types restriction may narrow it (null or empty config only).
    pull_request = on["pull_request"]
    assert pull_request is None or pull_request == {}, pull_request
    # push is restricted to the default branch and carries no other keys.
    assert on["push"] == {"branches": ["main"]}
    # merge_group must fire for every queue entry so the required check reports.
    assert on["merge_group"] == {"types": ["checks_requested"]}


def _setup_action() -> dict[str, object]:
    value = yaml.safe_load(SETUP_ACTION.read_text(encoding="utf-8"))
    assert isinstance(value, dict)
    return value


def _setup_action_steps() -> list[dict[str, object]]:
    runs = _setup_action().get("runs")
    assert isinstance(runs, dict)
    assert runs.get("using") == "composite"
    steps = runs.get("steps")
    assert isinstance(steps, list)
    assert all(isinstance(item, dict) for item in steps)
    return steps


def _change_filters(document: dict[str, object]) -> dict[str, list[object]]:
    step = next(
        candidate
        for candidate in _steps(_job(document, "detect-changes"))
        if candidate.get("id") == "filter"
    )
    options = step.get("with")
    assert isinstance(options, dict)
    raw = options.get("filters")
    assert isinstance(raw, str)
    parsed = yaml.safe_load(raw)
    assert isinstance(parsed, dict)
    assert all(isinstance(name, str) and isinstance(patterns, list) for name, patterns in parsed.items())
    return parsed


def _flatten_patterns(patterns: list[object]) -> list[str]:
    flattened: list[str] = []
    for pattern in patterns:
        if isinstance(pattern, list):
            flattened.extend(_flatten_patterns(pattern))
        else:
            assert isinstance(pattern, str)
            flattened.append(pattern)
    return flattened


def _matches_filter(path: str, pattern: str) -> bool:
    if pattern.endswith("/**"):
        prefix = pattern[:-3]
        return path == prefix or path.startswith(prefix + "/")
    assert "*" not in pattern, f"unsupported filter syntax must be modelled explicitly: {pattern}"
    return path == pattern


def test_ci_concurrency_replaces_stale_pending_main_runs() -> None:
    # Main pushes share a group with cancellation disabled. GitHub lets the
    # active run finish and replaces its pending run with the newest push.
    # Pull requests retain per-ref groups and cancel-in-progress behavior.
    document = _workflow()
    concurrency = document.get("concurrency")
    assert isinstance(concurrency, dict)
    assert concurrency.get("group") == (
        "${{ github.ref == 'refs/heads/main' && format('ci-{0}-main', github.workflow) "
        "|| format('ci-{0}-{1}', github.workflow, github.ref) }}"
    )
    assert concurrency.get("cancel-in-progress") == "${{ github.ref != 'refs/heads/main' }}"


def test_ci_uses_immutable_actions_and_least_privilege() -> None:
    document = _workflow()
    # Top-level grants nothing; each job declares its own least privilege.
    assert document.get("permissions") == {}
    # Trigger semantics (no path/branch restriction) are asserted
    # semantically in test_ci_triggers_carry_no_path_or_branch_restrictions.

    jobs = document.get("jobs")
    assert isinstance(jobs, dict)
    assert set(jobs) == set(JOB_PERMISSIONS)
    for job_name, job in jobs.items():
        assert isinstance(job, dict)
        assert job.get("permissions") == JOB_PERMISSIONS[job_name], job_name
        assert isinstance(job.get("timeout-minutes"), int), job_name
        for step in _steps(job):
            action = step.get("uses")
            if action is not None:
                assert isinstance(action, str)
                assert IMMUTABLE_ACTION.fullmatch(action) or LOCAL_ACTION.fullmatch(action), action
                if action.startswith("$/"):
                    assert (ROOT / action[2:] / "action.yml").is_file(), action
                if action.startswith("actions/checkout@"):
                    options = step.get("with")
                    assert isinstance(options, dict)
                    assert options.get("persist-credentials") is False
    for step in _setup_action_steps():
        action = step.get("uses")
        if action is not None:
            assert isinstance(action, str) and IMMUTABLE_ACTION.fullmatch(action), action


def test_every_tracked_path_matches_at_least_one_build_filter() -> None:
    document = _workflow()
    filters = _change_filters(document)
    outputs = _job(document, "detect-changes").get("outputs")
    assert isinstance(outputs, dict)
    assert set(outputs) == {
        "harness", "docs", "fabric", "split-root", "workflows", "rust", "lane", "tree", "passed-tree"
    }
    # `docs`, `tree` and `passed-tree` are computed from the PR file list or
    # tested checkout, not path filters. They do not cover tracked paths.
    patterns = [
        pattern
        for output in outputs
        if output in filters
        for pattern in _flatten_patterns(filters[output])
    ]
    tracked = subprocess.run(
        ["git", "ls-files"], cwd=ROOT, check=True, capture_output=True, text=True
    ).stdout.splitlines()
    unmatched = [
        path for path in tracked if not any(_matches_filter(path, pattern) for pattern in patterns)
    ]
    assert unmatched == [], f"tracked paths outside every build filter: {unmatched}"


def test_lane_conformance_runs_serially_on_linux_and_macos_for_lane_changes() -> None:
    document = _workflow()
    filters = _change_filters(document)
    patterns = _flatten_patterns(filters["lane"])
    for path in (
        "tools/front-door/src/lane.rs",
        "skills/orchestrate/scripts/cf_dispatch.py",
        "skills/orchestrate/scripts/dispatch_run.py",
        "skills/orchestrate/scripts/provider_exec.py",
        "skills/orchestrate/scripts/run_controls.py",
        "skills/orchestrate/scripts/run_lineage.py",
        "skills/orchestrate/scripts/run_dir_init.sh",
        "skills/_shared/bounded_process.py",
        "skills/_shared/custody.py",
        "skills/_shared/process_info.py",
        "skills/_shared/attempt_storage.py",
        "skills/_shared/attempt_evidence.py",
        "skills/_shared/memory_admission.py",
        "skills/_shared/layout.py",
        "tests/lane_conformance/test_seed.py",
        "tests/support/dispatch.py",
        "tests/support/proc.py",
        "tests/support/runs.py",
        "tests/support/fake_cli.py",
        "tests/support/git.py",
        "config/model-routing.json",
        "scripts/lib/jsonc.py",
        "scripts/fabric_hosts.py",
        "tests/cf_dispatch_support.py",
        "tests/conftest.py",
        ".github/workflows/ci.yml",
    ):
        assert any(_matches_filter(path, pattern) for pattern in patterns), path
    assert not any(_matches_filter("docs/unrelated.md", pattern) for pattern in patterns)
    job = _job(document, "lane-conformance")
    assert job["needs"] == "detect-changes"
    assert job["if"] == "needs.detect-changes.outputs.lane == 'true'"
    assert job["runs-on"] == "${{ matrix.os }}"
    assert job["strategy"]["matrix"]["os"] == ["ubuntu-latest", "macos-15"]
    commands = [step["run"] for step in _steps(job) if "run" in step]
    assert commands == ["uv sync --locked --only-group test",
                        ".venv/bin/python -m pytest tests/lane_conformance -q"]
    shards = "\n".join(str(step.get("run", "")) for step in _steps(_job(document, "python-tests")))
    assert "--ignore=tests/lane_conformance" in shards


def test_macos_smoke_builds_the_release_front_door_with_the_rust_filter() -> None:
    document = _workflow()
    job = _job(document, "rust-macos-release")
    assert job["needs"] == "detect-changes"
    assert job["if"] == _job(document, "rust")["if"]
    assert job["runs-on"] == "macos-latest"
    patterns = _flatten_patterns(_change_filters(document)["rust"])
    assert any(_matches_filter("tools/front-door/crates/cli/src/lib.rs", p) for p in patterns)
    builds = [step for step in _steps(job) if "cargo build" in str(step.get("run", ""))]
    assert len(builds) == 1
    assert builds[0]["working-directory"] == "tools/front-door"
    assert "cargo build --release --locked -p provenant --bin provenant" in builds[0]["run"]
    assert not any("cargo test" in str(step.get("run", "")) for step in _steps(job))


def test_ci_status_depends_on_every_other_job_and_fails_closed() -> None:
    document = _workflow()
    jobs = document.get("jobs")
    assert isinstance(jobs, dict)
    aggregate = _job(document, "ci-status")
    assert set(aggregate.get("needs", [])) == set(jobs) - {"ci-status"}
    assert aggregate.get("if") == "always()"
    commands = "\n".join(str(step.get("run", "")) for step in _steps(aggregate))
    assert '.value.result != "success"' in commands
    assert '.value.result != "skipped"' in commands
    assert "exit 1" in commands


def test_no_job_or_step_sets_continue_on_error() -> None:
    # PR #168 repair cycle 3 (P2): continue-on-error on any required job or
    # step would neutralise failure propagation invisibly.
    _assert_no_continue_on_error(_workflow())


def test_continue_on_error_on_a_step_fails_the_guard() -> None:
    document = _workflow()
    steps = _steps(_job(document, "ci-status"))
    steps[0]["continue-on-error"] = True
    with pytest.raises(AssertionError):
        _assert_no_continue_on_error(document)


def test_continue_on_error_on_a_job_fails_the_guard() -> None:
    document = _workflow()
    _job(document, "fabric")["continue-on-error"] = True
    with pytest.raises(AssertionError):
        _assert_no_continue_on_error(document)


def test_continue_on_error_inside_the_shared_setup_action_fails_the_guard() -> None:
    # Prior to this guard covering the composite action, a continue-on-error
    # planted in setup-node-workspace's own steps was outside every mutation
    # test above -- none of them ever parsed or asserted on that file -- so
    # this was a real, previously unenforced blind spot, not a hypothetical
    # one.
    document = _workflow()
    setup_steps = _setup_action_steps()
    setup_steps[-1]["continue-on-error"] = True
    with pytest.raises(AssertionError):
        _assert_no_continue_on_error(document, setup_steps)


def test_ci_triggers_carry_no_path_or_branch_restrictions() -> None:
    # PR #168 repair cycle 3 (P2): parse `on:` semantically so a push.paths
    # or pull_request.paths restriction cannot slip past a substring check.
    _assert_trigger_semantics(_workflow())


@pytest.mark.parametrize(
    "restriction",
    [
        {"paths": ["runtime/**"]},
        {"paths-ignore": ["docs/**"]},
        {"branches": ["main"]},
        {"types": ["opened", "synchronize"]},
    ],
)
def test_pull_request_trigger_restriction_fails_the_guard(restriction: dict[str, object]) -> None:
    document = _workflow()
    _triggers(document)["pull_request"] = restriction
    with pytest.raises(AssertionError):
        _assert_trigger_semantics(document)


def test_push_trigger_extra_key_fails_the_guard() -> None:
    document = _workflow()
    _triggers(document)["push"] = {"branches": ["main"], "paths": ["runtime/**"]}
    with pytest.raises(AssertionError):
        _assert_trigger_semantics(document)


def test_exact_on_key_guard_passes_for_the_real_workflow() -> None:
    _assert_exact_on_key(WORKFLOW.read_text(encoding="utf-8"))


@pytest.mark.parametrize("spelling", ["On", "ON", "oN", "True", "TRUE", "Yes"])
def test_on_key_casing_typo_fails_the_exact_key_guard(spelling: str) -> None:
    # GitHub requires the exact lowercase string "on" and silently ignores
    # any other spelling, so every one of these casing typos must fail the
    # raw-text guard even though at least the exact-cased YAML-1.1 boolean
    # spellings ("On", "ON", "True", ...) would parse cleanly through
    # _triggers (the parsed document cannot tell them apart from a correct
    # `on:` -- that is the whole reason this guard reads the raw text
    # instead).
    text = re.sub(r"(?m)^on:", f"{spelling}:", WORKFLOW.read_text(encoding="utf-8"), count=1)
    with pytest.raises(AssertionError):
        _assert_exact_on_key(text)


def test_duplicate_on_key_boolean_alias_fails_the_exact_key_guard() -> None:
    # A real `on:` alongside a stray bare `On:` typo are two distinct
    # PyYAML keys (string "on" vs boolean True) rather than a collision, so
    # both would survive parsing simultaneously and the old
    # `document.get(True, document.get("on"))` helper preferred the typo's
    # (boolean-keyed) value over the real one. The raw-text guard rejects
    # this shape outright instead of silently picking either key.
    text = WORKFLOW.read_text(encoding="utf-8")
    on_block = "on:\n  push:\n    branches: [main]\n  pull_request:\n"
    assert on_block in text
    mutated = text.replace(on_block, on_block + "On:\n  workflow_dispatch: {}\n", 1)
    with pytest.raises(AssertionError):
        _assert_exact_on_key(mutated)


def test_ci_runs_complete_harness_and_fabric_gates() -> None:
    document = _workflow()
    harness_steps = _steps(_job(document, "harness-checks"))
    python_test_job = _job(document, "python-tests")
    python_test_steps = _steps(python_test_job)
    fabric_steps = _steps(_job(document, "fabric"))

    # The toolchain contract (Node 24, pinned npm before a locked install)
    # moved into the shared composite action; assert it once there, then
    # assert every workspace job consumes the composite after checkout.
    setup_steps = _setup_action_steps()
    node_setup = next(
        step for step in setup_steps if str(step.get("uses", "")).startswith("actions/setup-node@")
    )
    assert node_setup.get("with", {}).get("node-version") == "24"
    setup_commands = [str(step.get("run", "")) for step in setup_steps]
    pin_index = next(
        index for index, command in enumerate(setup_commands) if "npm install --global npm@11.12.1" in command
    )
    install_index = next(
        index for index, command in enumerate(setup_commands)
        if "retry-npm-install.py -- npm ci" in command
    )
    assert pin_index < install_index

    for job_name in ("harness-checks", "python-tests", "fabric", "split-root"):
        steps = _steps(_job(document, job_name))
        uses = [str(step.get("uses", "")) for step in steps]
        checkout_index = next(index for index, action in enumerate(uses) if action.startswith("actions/checkout@"))
        composite_index = uses.index(SETUP_ACTION_USES)
        assert checkout_index < composite_index, job_name

    harness_commands = "\n".join(str(step.get("run", "")) for step in harness_steps)
    assert "scripts/check-harness --checks-only" in harness_commands
    assert "runtime/agent-fabric" not in harness_commands
    assert python_test_job.get("strategy", {}).get("matrix", {}).get("shard") == [
        "fabric-hosts",
        "python-a",
        "python-b",
    ]
    assert not (ROOT / ".github/workflows/python-test-shards.json").exists()
    assert not (ROOT / "scripts/check-python-test-shards.py").exists()
    shard_run = next(step for step in python_test_steps if step.get("name") == "Run Python test shard")
    assert shard_run.get("env", {}).get("PROVENANT_TEST_SHARD") == (
        "${{ matrix.shard == 'fabric-hosts' && '1/3' || matrix.shard == 'python-a' && '2/3' || '3/3' }}"
    )
    assert "-m pytest -q --durations=30 --durations-min=1 -n auto" in shard_run.get("run", "")
    hook = (ROOT / "tests" / "conftest.py").read_text(encoding="utf-8")
    assert "PROVENANT_TEST_SHARD" in hook
    assert "zlib.crc32" in hook
    assert '"tests/test_fabric_hosts.py"' in hook

    tree_check = next(
        step for step in _steps(_job(document, "detect-changes"))
        if step.get("name") == "Check for a passing PR run on this tree"
    )
    assert "git rev-parse 'HEAD^{tree}'" in tree_check.get("run", "")
    assert "gh api \"repos/$REPO/actions/artifacts?name=$name\"" in tree_check.get("run", "")
    assert '"passed-tree=false"' in tree_check.get("run", "")
    assert "needs.detect-changes.outputs.passed-tree != 'true'" in _job(document, "python-tests").get("if", "")
    ci_status_steps = _steps(_job(document, "ci-status"))
    marker_upload = next(step for step in ci_status_steps if step.get("name") == "Record passing PR tree")
    assert marker_upload.get("if") == (
        "github.event_name == 'pull_request' && needs.detect-changes.outputs.harness == 'true'"
    )
    assert marker_upload.get("with", {}).get("name") == (
        "ci-passed-tree-${{ needs.detect-changes.outputs.tree }}"
    )
    assert str(marker_upload.get("uses", "")).startswith("actions/upload-artifact@")

    fabric_commands = "\n".join(str(step.get("run", "")) for step in fabric_steps)
    for required in (
        "npm run check",
        "npm run test:package-install --workspace @local/fabric",
        "node runtime/fabric/mcp-smoke.mjs",
        "npm audit --workspace=@local/fabric --omit=dev --audit-level=high",
    ):
        assert required in fabric_commands
    assert "runtime/agent-fabric" not in fabric_commands
    run_steps = [step for step in fabric_steps if "run" in step]
    assert all("working-directory" not in step for step in run_steps)


def test_harness_python_test_dependencies_install_locked_and_cached() -> None:
    # Issue #200: the harness job's Python test tooling (pytest, PyYAML)
    # installs from the committed uv.lock — never from an unconstrained
    # `pip install` — and the setup-uv cache is keyed on that lock so a
    # lock change invalidates the cache. The lock is the pin: pyproject
    # declares the `test` dependency group, uv.lock pins it transitively.
    document = _workflow()
    harness_steps = _steps(_job(document, "harness-checks"))

    setup_uv = next(
        step for step in harness_steps if str(step.get("uses", "")).startswith("astral-sh/setup-uv@")
    )
    options = setup_uv.get("with")
    assert isinstance(options, dict)
    assert options.get("python-version") == "3.14"
    assert options.get("enable-cache") is True
    assert options.get("cache-dependency-glob") == "uv.lock"

    run_commands = [str(step.get("run", "")).strip() for step in harness_steps if "run" in step]
    assert not any("pip install" in command for command in run_commands)
    sync_index = run_commands.index("uv sync --locked --only-group test")
    gate_index = run_commands.index("scripts/check-harness --checks-only")
    assert sync_index < gate_index

    # The gate runs against the synced environment: scripts/check-harness
    # honours HARNESS_PYTHON before any interpreter fallback.
    gate_step = next(
        step for step in harness_steps if str(step.get("run", "")).strip() == "scripts/check-harness --checks-only"
    )
    assert gate_step.get("env") == {
        "AGENT_FABRIC_PRODUCT_ROOT": "${{ github.workspace }}",
        "AGENT_FABRIC_INSTANCE_ROOT": "${{ github.workspace }}",
        "HARNESS_PYTHON": "${{ github.workspace }}/.venv/bin/python",
    }

    pyproject = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))
    # Not a Python package: uv must never build or install the repo itself.
    assert pyproject["tool"]["uv"]["package"] is False
    declared = {
        re.split(r"[<>=~!\[; ]", requirement, maxsplit=1)[0]
        for requirement in pyproject["dependency-groups"]["test"]
    }
    assert declared == {"jsonschema", "pytest", "pytest-xdist", "pyyaml", "rfc3339-validator"}

    lock_text = (ROOT / "uv.lock").read_text(encoding="utf-8")
    locked = set(re.findall(r'(?m)^name = "([^"]+)"$', lock_text))
    assert {"pytest", "pyyaml"} <= locked


def test_fabric_workspace_and_ci_share_the_locked_daemonless_check_graph() -> None:
    document = _workflow()
    fabric_steps = _steps(_job(document, "fabric"))
    fabric_paths = set(_flatten_patterns(_change_filters(document)["fabric"]))
    assert {
        "config/**",
        "pyproject.toml",
        "scripts/**",
        "skills/**",
        "uv.lock",
    } <= fabric_paths
    node_setup = next(
        step for step in _setup_action_steps() if str(step.get("uses", "")).startswith("actions/setup-node@")
    )
    cache_paths = str(node_setup.get("with", {}).get("cache-dependency-path", "")).splitlines()
    assert cache_paths == ["package-lock.json"]

    root_package = json.loads(ROOT_PACKAGE.read_text(encoding="utf-8"))
    assert root_package.get("workspaces") == ["runtime/fabric"]
    root_scripts = root_package.get("scripts")
    assert isinstance(root_scripts, dict)
    assert root_scripts.get("check") == (
        "python3 scripts/node-workspace-provision.py && "
        "node scripts/node-workspace-preflight.mjs && npm run typecheck && npm run test"
    )
    assert root_scripts.get("typecheck") == "npm run typecheck --workspaces --if-present"
    assert root_scripts.get("test") == "npm run test --workspaces --if-present"
    assert ROOT_LOCK.is_file()
    assert not list((ROOT / "runtime").glob("*/package-lock.json"))
    root_dev_dependencies = root_package.get("devDependencies")
    assert isinstance(root_dev_dependencies, dict)
    assert isinstance(root_dev_dependencies.get("tsx"), str)
    assert root_package.get("dependencies") is None

    package = json.loads(FABRIC_PACKAGE.read_text(encoding="utf-8"))
    assert package.get("name") == "@local/fabric"
    assert package.get("engines") == {"node": ">=24.15.0 <25"}
    scripts = package.get("scripts")
    assert isinstance(scripts, dict)
    assert {"test", "test:package-install", "typecheck"} <= scripts.keys()
    dependencies = package.get("dependencies")
    assert isinstance(dependencies, dict)
    assert isinstance(dependencies.get("tsx"), str)

    setup_uv = next(
        step for step in fabric_steps if str(step.get("uses", "")).startswith("astral-sh/setup-uv@")
    )
    assert setup_uv.get("with") == {
        "python-version": "3.14",
        "enable-cache": True,
        "cache-dependency-glob": "uv.lock",
    }
    smoke_step = next(
        step for step in fabric_steps
        if str(step.get("run", "")).strip() == "node runtime/fabric/mcp-smoke.mjs"
    )
    assert smoke_step.get("env") == {
        "HARNESS_PYTHON": "${{ github.workspace }}/.venv/bin/python",
    }

    fabric_commands = "\n".join(str(step.get("run", "")) for step in fabric_steps)
    assert fabric_commands.index("uv sync --locked --only-group test") < fabric_commands.index("npm run check")


def test_repository_policy_covers_sensitive_fabric_surfaces() -> None:
    # Issue #150: a single-maintainer repository gains nothing from
    # per-directory rules that all name the same owner; the wildcard is the
    # whole policy and keeps CODEOWNERS from drifting as directories move.
    codeowners = (ROOT / ".github" / "CODEOWNERS").read_text(encoding="utf-8")
    rules = [line for line in codeowners.splitlines() if line.strip() and not line.startswith("#")]
    assert rules == ["* @mblauberg"]

    dependabot = yaml.safe_load((ROOT / ".github" / "dependabot.yml").read_text(encoding="utf-8"))
    updates = dependabot.get("updates", [])
    npm_updates = [item for item in updates if item.get("package-ecosystem") == "npm"]
    assert any(item.get("directory") == "/" for item in npm_updates)
    assert any(item.get("package-ecosystem") == "github-actions" and item.get("directory") == "/" for item in updates)

    template = (ROOT / ".github" / "pull_request_template.md").read_text(encoding="utf-8").lower()
    # Evidence must be bindable to the reviewed commit.
    assert "head sha" in template
    assert "- [ ]" not in template


def test_dependabot_automerge_excludes_primary_provider_packages() -> None:
    # Issues #195 and #208: these packages affect runtime adapter conformance,
    # so even patch updates require compatibility review.
    document = _parse_workflow_text(
        (ROOT / ".github" / "workflows" / "dependabot-automerge.yml").read_text(encoding="utf-8")
    )
    steps = _steps(_job(document, "automerge"))
    merge_step = next(step for step in steps if "gh pr merge" in str(step.get("run", "")))
    merge_condition = str(merge_step.get("if", ""))
    assert "steps.metadata.outputs.update-type == 'version-update:semver-patch'" in merge_condition
    excluded = (
        "@anthropic-ai/claude-agent-sdk",
        "@anthropic-ai/claude-code",
        "@openai/codex",
    )
    for dependency in excluded:
        assert f"!contains(steps.metadata.outputs.dependency-names, '{dependency}')" in merge_condition
    skip_step = next(step for step in steps if "Skipping auto-merge" in str(step.get("run", "")))
    skip_condition = str(skip_step.get("if", ""))
    assert "steps.metadata.outputs.update-type != 'version-update:semver-patch'" in skip_condition
    assert "!contains" not in skip_condition
    for dependency in excluded:
        assert f"contains(steps.metadata.outputs.dependency-names, '{dependency}')" in skip_condition


def test_work_item_form_exposes_intake_fields() -> None:
    form = yaml.safe_load(
        (ROOT / ".github" / "ISSUE_TEMPLATE" / "work-item.yml").read_text(encoding="utf-8")
    )
    field_ids = {item.get("id") for item in form["body"] if "id" in item}
    assert {"problem-evidence", "outcome", "scope", "acceptance", "dependencies", "risk-authority-gates"} <= field_ids


def test_gate_reports_each_step_and_tails_failed_step_log(tmp_path: Path) -> None:
    root = tmp_path / "product"
    scripts = root / "scripts"
    scripts.mkdir(parents=True)
    gate = scripts / "gate"
    gate.write_bytes((ROOT / "scripts" / "gate").read_bytes())
    gate.chmod(0o755)

    path_bin = tmp_path / "bin"
    path_bin.mkdir()
    for name in ("npm", "node", "git"):
        executable = path_bin / name
        executable.write_text(
            "#!/bin/sh\n"
            "i=1\n"
            "while [ $i -le 25 ]; do echo \"$0 line-$i\"; i=$((i + 1)); done\n"
            + ("exit 7\n" if name == "node" else "exit 0\n"),
            encoding="utf-8",
        )
        executable.chmod(0o755)
    for name in ("check-harness", "static-security-check.py", "public-release-check"):
        executable = scripts / name
        executable.write_text("#!/bin/sh\necho \"$0 ok\"\nexit 0\n", encoding="utf-8")
        executable.chmod(0o755)

    result = subprocess.run(
        [str(gate)],
        cwd=tmp_path,
        env={
            **os.environ,
            "AGENT_FABRIC_PRODUCT_ROOT": str(root),
            "PATH": f"{path_bin}:{os.environ['PATH']}",
            "GATE_LOG_DIR": str(tmp_path / "logs"),
        },
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 1
    lines = result.stdout.splitlines()
    summaries = [line for line in lines if re.match(r"(?:ok|FAIL) [a-z0-9-]+ \d+s$", line)]
    failures = [line for line in summaries if line.startswith("FAIL ")]
    assert len(failures) == 1
    assert "fabric-mcp-smoke" in failures[0]
    assert "line-25" in result.stdout
    failed_log_line = next(line for line in lines if line.startswith("log: "))
    failed_log = Path(failed_log_line.removeprefix("log: "))
    assert failed_log.parent.parent == tmp_path / "logs"
    assert failed_log.suffix == ".log"
    assert failed_log.is_file()
