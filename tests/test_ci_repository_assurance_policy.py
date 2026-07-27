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
import re
import subprocess
import tomllib
from pathlib import Path

import pytest
import yaml


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
SETUP_ACTION = ROOT / ".github" / "actions" / "setup-node-workspace" / "action.yml"
SETUP_ACTION_USES = "./.github/actions/setup-node-workspace"
ROOT_PACKAGE = ROOT / "package.json"
ROOT_LOCK = ROOT / "package-lock.json"
PROTOCOL_PACKAGE = ROOT / "runtime" / "agent-fabric-protocol" / "package.json"
PROTOCOL_SCHEMA_WRITER = (
    ROOT / "runtime" / "agent-fabric-protocol" / "scripts" / "write-schema.mjs"
)
FABRIC_PACKAGE = ROOT / "runtime" / "agent-fabric" / "package.json"
IMMUTABLE_ACTION = re.compile(r"^[^@\s]+@[0-9a-f]{40}$")
# Local composite actions are pinned by the commit under review itself; only
# repository-local paths are exempt from the 40-hex SHA pin.
LOCAL_ACTION = re.compile(r"^\./\.github/actions/[a-z0-9-]+$")
# Path filtering (issue #150): build jobs are gated per-path by
# detect-changes and a single always-run ci-status aggregate is the one
# required check, so a skipped job can never leave a required check pending.
FILTERED_JOBS = {
    "harness": "harness",
    "fabric": "fabric",
    "console": "console",
    "herdr": "herdr",
    "review-portal-supervisor": "review-portal-supervisor",
    "zizmor": "workflows",
}
# PR #168 repair cycle 3: the filters block defines two YAML-anchor helper
# arms (ci-contract, node-workspace) that exist only to be aliased into the
# consumed job filters; no detect-changes output reads them directly. Every
# other filter key must be consumed by a job-driving output, so an orphan
# filter arm that gates nothing can never satisfy the tracked-file closure
# oracle in test_every_tracked_file_matches_at_least_one_path_filter.
FILTER_HELPER_KEYS = frozenset({"ci-contract", "node-workspace"})
# PR #168 repair cycle 2: fabric tests execute exactly these repo files
# outside runtime/agent-fabric — the model-routing acceptance suite runs
# scripts/model-route (a bash wrapper that execs scripts/model_route.py,
# where the logic lives) and the delivery-run fixture runs
# skills/deliver/scripts/*.py — so those paths must retrigger the fabric
# job. Repair cycle 3 added scripts/model_route.py: the wrapper was already
# listed but the module it execs was not, so a PR changing only the Python
# module skipped the fabric job that runs it. This allowlist is exact by
# design: adding an executed dependency means adding it here deliberately,
# and nothing else in docs/ or skills/ may path-trigger a non-harness job.
# PR #446 added scripts/model_route_catalog.py, which model_route.py loads and
# executes at import: a change to it alone reaches the fabric suite at runtime,
# so it must retrigger the job that runs it for the same reason cycle 3 added
# model_route.py itself. Issue #445 adds model_route_preferences.py under the
# same path-loaded execution convention.
FABRIC_EXECUTED_DEPENDENCIES = frozenset(
    {
        "scripts/model-route",
        "scripts/model_route.py",
        "scripts/model_route_catalog.py",
        "scripts/model_route_preferences.py",
        "skills/deliver/scripts/**",
    }
)
JOB_PERMISSIONS = {
    "detect-changes": {"pull-requests": "read"},
    "harness": {"contents": "read"},
    "fabric": {"contents": "read"},
    "review-portal-supervisor": {"contents": "read"},
    "console": {"contents": "read"},
    "herdr": {"contents": "read"},
    "zizmor": {"contents": "read"},
    "ci-status": {},
}
WORKSPACE_GUIDES = (
    ROOT / "docs" / "runbooks" / "agent-fabric-operations.md",
    ROOT / "docs" / "runbooks" / "agent-fabric-traceability.md",
    ROOT / "runtime" / "agent-fabric" / "README.md",
    ROOT / "runtime" / "agent-fabric-herdr" / "README.md",
)


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


def _flatten_rules(rules: object) -> list[str]:
    flat: list[str] = []
    assert isinstance(rules, list)
    for rule in rules:
        if isinstance(rule, list):
            flat.extend(_flatten_rules(rule))
        else:
            assert isinstance(rule, str)
            flat.append(rule)
    return flat


def _path_filters(document: dict[str, object]) -> dict[str, list[str]]:
    # yaml.safe_load resolves the &anchors/*aliases in the filters block, so
    # shared rule groups arrive as nested lists and are flattened here.
    detect = _job(document, "detect-changes")
    filter_step = next(
        step for step in _steps(detect) if str(step.get("uses", "")).startswith("dorny/paths-filter@")
    )
    filters = yaml.safe_load(str(filter_step.get("with", {}).get("filters")))
    assert isinstance(filters, dict)
    return {name: _flatten_rules(rules) for name, rules in filters.items()}


def _consumed_filter_names(document: dict[str, object]) -> set[str]:
    # The filter names a job-driving output actually reads: every
    # detect-changes output forwards exactly one steps.filter.outputs.<name>,
    # so the consumed set is derived from the outputs, never assumed.
    detect = _job(document, "detect-changes")
    outputs = detect.get("outputs")
    assert isinstance(outputs, dict)
    consumed: set[str] = set()
    for expr in outputs.values():
        match = re.search(r"steps\.filter\.outputs\.([a-z0-9-]+)", str(expr))
        assert match, expr
        consumed.add(match.group(1))
    return consumed


def _assert_all_filters_consumed(document: dict[str, object]) -> None:
    # Every filter key is either a job-consumed filter or a declared helper
    # anchor; an orphan arm that no output reads must not exist.
    flattened = _path_filters(document)
    consumed = _consumed_filter_names(document)
    filter_keys = set(flattened)
    assert filter_keys - FILTER_HELPER_KEYS == consumed
    assert not (FILTER_HELPER_KEYS & consumed)


def _inject_filter(document: dict[str, object], name: str, rules: list[str]) -> None:
    # Add a filter arm to the parsed workflow by rewriting the paths-filter
    # `with.filters` block, so _path_filters re-parses it exactly as the
    # action would.
    detect = _job(document, "detect-changes")
    filter_step = next(
        step for step in _steps(detect) if str(step.get("uses", "")).startswith("dorny/paths-filter@")
    )
    with_block = filter_step.get("with")
    assert isinstance(with_block, dict)
    filters = yaml.safe_load(str(with_block.get("filters")))
    assert isinstance(filters, dict)
    filters[name] = rules
    with_block["filters"] = yaml.safe_dump(filters)


def _jobs_for_changed_paths(document: dict[str, object], paths: list[str]) -> set[str]:
    # Simulate dorny/paths-filter: a job is selected when any changed path
    # matches any rule in the filter its detect-changes output consumes.
    flattened = _path_filters(document)
    selected: set[str] = set()
    for job_name, output_name in FILTERED_JOBS.items():
        rules = flattened[output_name]
        if any(_filter_matches(rule, path) for rule in rules for path in paths):
            selected.add(job_name)
    return selected


def _assert_no_continue_on_error(
    document: dict[str, object],
    setup_steps: list[dict[str, object]] | None = None,
) -> None:
    # continue-on-error on any required job or step would neutralise failure
    # propagation invisibly, so no job and no step may set it. This also
    # covers the shared composite action (setup-node-workspace) that every
    # workspace job runs through via `uses: ./...` -- a continue-on-error
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
    assert set(on) == {"push", "pull_request"}
    # pull_request must fire on every PR: no paths, paths-ignore, branches or
    # types restriction may narrow it (null or empty config only).
    pull_request = on["pull_request"]
    assert pull_request is None or pull_request == {}, pull_request
    # push is restricted to the default branch and carries no other keys.
    assert on["push"] == {"branches": ["main"]}


def _filter_matches(pattern: str, path: str) -> bool:
    # The filters block deliberately uses only two glob forms: exact tracked
    # paths and directory prefixes written as `prefix/**`. Fail loudly on any
    # other form so unsupported syntax cannot silently defeat the coverage
    # assertion below.
    if pattern.endswith("/**"):
        prefix = pattern[: -len("/**")]
        assert prefix and not any(ch in prefix for ch in "*?["), f"unsupported glob form: {pattern}"
        return path == prefix or path.startswith(prefix + "/")
    assert not any(ch in pattern for ch in "*?["), f"unsupported glob form: {pattern}"
    return path == pattern


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


def test_ci_concurrency_policy_exempts_main_from_cancellation() -> None:
    # Issue #170 (deferred from #150/#168): pushes to main force every
    # path-filter output true, so ci-status — the single required check
    # pinned on the branch protection ruleset — always runs on a main push.
    # cancel-in-progress must stay true for PR runs (superseded runs are
    # disposable) but must not cancel main-push runs: a cancelled main run
    # would leave the merged commit's ci-status at "cancelled" forever, since
    # main is never re-pushed to retrigger it — the commit's regression
    # signal is silently lost. PR merging itself is unaffected (required
    # checks are evaluated on the PR head, not main's commits); the exemption
    # protects main's audit trail.
    document = _workflow()
    concurrency = document.get("concurrency")
    assert isinstance(concurrency, dict)
    assert concurrency.get("group") == "ci-${{ github.workflow }}-${{ github.ref }}"
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
                if action.startswith("./"):
                    assert (ROOT / action[2:] / "action.yml").is_file(), action
                if action.startswith("actions/checkout@"):
                    options = step.get("with")
                    assert isinstance(options, dict)
                    assert options.get("persist-credentials") is False
    for step in _setup_action_steps():
        action = step.get("uses")
        if action is not None:
            assert isinstance(action, str) and IMMUTABLE_ACTION.fullmatch(action), action


def test_ci_gates_build_jobs_behind_path_filters_and_one_aggregate_check() -> None:
    document = _workflow()

    detect = _job(document, "detect-changes")
    filter_step = next(
        step for step in _steps(detect) if str(step.get("uses", "")).startswith("dorny/paths-filter@")
    )
    # The filter only inspects pull requests; pushes to main force-run all
    # jobs through the job outputs below.
    assert filter_step.get("if") == "github.event_name == 'pull_request'"
    flattened = _path_filters(document)
    # PR #168 repair cycle 1: the harness filter must cover every surface
    # scripts/check-harness validates, so skills-, docs- and root-document
    # changes always run the policy gate. Repair cycle 2 added the repo-wide
    # scan residue (CHANGELOG.md, .gitignore): scripts/check-harness scans
    # every tracked file and tests/test_harness_contract.py validates
    # .gitignore, so those files must retrigger the harness job too.
    for required in (
        "skills/**",
        "docs/**",
        ".gitignore",
        "AGENTS.md",
        "CHANGELOG.md",
        "HARNESS.md",
        "MAINTAINING.md",
        "README.md",
        "LICENSES/**",
        "NOTICE",
        "SECURITY.md",
        "THIRD_PARTY_NOTICES.md",
        # Issue #200: the locked Python test tooling gates the harness job
        # itself, so a lock or manifest change must re-run that job.
        "pyproject.toml",
        "uv.lock",
    ):
        assert required in flattened["harness"], required
    # Non-harness jobs must not path-trigger on docs/ or skills/ broadly.
    # The only exception is the exact executed-dependency allowlist for the
    # fabric job (repair cycle 2): the blanket prohibition is narrowed, not
    # deleted, and any new entry must be added to the allowlist deliberately.
    assert FABRIC_EXECUTED_DEPENDENCIES == {
        "scripts/model-route",
        "scripts/model_route.py",
        "scripts/model_route_catalog.py",
        "scripts/model_route_preferences.py",
        "skills/deliver/scripts/**",
    }
    assert FABRIC_EXECUTED_DEPENDENCIES <= set(flattened["fabric"])
    for name, rules in flattened.items():
        if name == "harness":
            continue
        allowed = FABRIC_EXECUTED_DEPENDENCIES if name == "fabric" else frozenset()
        docs_skills = {rule for rule in rules if rule.startswith(("docs/", "skills/"))}
        assert docs_skills <= allowed, name
    # Fabric tests read config/ fixtures (review profiles, acceptance
    # scenarios, agent-fabric.yaml), so config changes rerun the fabric job.
    assert "config/**" in flattened["fabric"]
    # A change to the CI contract itself re-runs every gated job. The
    # workflows gate uses the broader glob, which covers ci.yml.
    for gate in FILTERED_JOBS.values():
        assert ".github/actions/**" in flattened[gate]
        if gate == "workflows":
            assert ".github/workflows/**" in flattened[gate]
        else:
            assert ".github/workflows/ci.yml" in flattened[gate]
    assert "runtime/**" in flattened["harness"]
    assert "tests/**" in flattened["harness"]
    for gate in ("harness", "fabric", "console", "herdr"):
        assert "package-lock.json" in flattened[gate]
        assert "runtime/agent-fabric-protocol/**" in flattened[gate]
    assert "runtime/agent-fabric/**" in flattened["console"]
    assert "runtime/agent-fabric-review-portal-supervisor/**" in flattened["review-portal-supervisor"]

    outputs = detect.get("outputs")
    assert isinstance(outputs, dict)
    for output_name in FILTERED_JOBS.values():
        assert outputs.get(output_name) == (
            "${{ github.event_name == 'push' && 'true' || steps.filter.outputs." + output_name + " }}"
        )

    for job_name, output_name in FILTERED_JOBS.items():
        job = _job(document, job_name)
        assert job.get("needs") == "detect-changes", job_name
        assert job.get("if") == f"needs.detect-changes.outputs.{output_name} == 'true'", job_name

    aggregate = _job(document, "ci-status")
    # ci-status is the single required check: it must always report, and it
    # must fail closed on any needed job that failed or was cancelled.
    assert aggregate.get("if") == "always()"
    assert set(aggregate.get("needs", [])) == {"detect-changes", *FILTERED_JOBS}
    # Every job in the workflow except the aggregate itself must be in its
    # needs list, so a job added later cannot silently bypass the required
    # check.
    jobs = document.get("jobs")
    assert isinstance(jobs, dict)
    assert set(aggregate.get("needs", [])) == set(jobs) - {"ci-status"}
    (status_step,) = _steps(aggregate)
    assert status_step.get("env") == {"NEEDS_JSON": "${{ toJSON(needs) }}"}
    command = str(status_step.get("run", ""))
    assert '.value.result != "success" and .value.result != "skipped"' in command
    assert "exit 1" in command


def test_every_tracked_file_matches_at_least_one_path_filter() -> None:
    # PR #168 repair cycle 2 (Findings A and C): scripts/check-harness scans
    # every tracked file, so a tracked file outside every filter would let a
    # PR touching only that file skip all build jobs, pass the all-skipped
    # ci-status aggregate, and break the next push to main. This closure
    # assertion makes such residue impossible to reintroduce.
    #
    # Repair cycle 3 (orphan-filter finding): close over only the filters a
    # job-driving output actually consumes, so an orphan filter arm that
    # gates nothing can never satisfy this oracle.
    document = _workflow()
    flattened = _path_filters(document)
    consumed = _consumed_filter_names(document)
    arms = sorted({rule for name in consumed for rule in flattened[name]})
    tracked = subprocess.run(
        ["git", "ls-files"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.splitlines()
    assert tracked
    unmatched = [path for path in tracked if not any(_filter_matches(arm, path) for arm in arms)]
    assert unmatched == [], (
        "tracked files outside every CI path filter; add each one to the "
        f"filter of the job that validates it: {unmatched}"
    )


def test_path_filters_are_all_consumed_by_a_job_driving_output() -> None:
    # PR #168 repair cycle 3: the coverage-closure oracle unions only the
    # filters that detect-changes outputs consume. Assert that consumed set
    # equals the FILTERED_JOBS output names, that each is read by exactly the
    # jobs mapped to it, and that no filter key outside the helper anchors is
    # left unconsumed — an orphan arm would gate nothing yet could satisfy
    # closure.
    document = _workflow()
    flattened = _path_filters(document)
    consumed = _consumed_filter_names(document)
    assert consumed == set(FILTERED_JOBS.values())
    for name in consumed:
        consuming_jobs = {job for job, output in FILTERED_JOBS.items() if output == name}
        assert len(consuming_jobs) == 1, name
    filter_keys = set(flattened)
    assert FILTER_HELPER_KEYS <= filter_keys
    assert filter_keys - FILTER_HELPER_KEYS == consumed
    _assert_all_filters_consumed(document)


def test_orphan_filter_arm_fails_consumption_and_gates_nothing() -> None:
    # Mutation: an orphan filter arm no output consumes must fail the
    # consumption assertion and must not enter the closure arms.
    document = _workflow()
    _inject_filter(document, "orphan", ["new-surface.txt"])
    with pytest.raises(AssertionError):
        _assert_all_filters_consumed(document)
    flattened = _path_filters(document)
    consumed = _consumed_filter_names(document)
    closure_arms = {rule for name in consumed for rule in flattened[name]}
    assert "new-surface.txt" not in closure_arms


def test_model_route_python_change_selects_the_fabric_job() -> None:
    # PR #168 repair cycle 3 (P1): scripts/model-route is a bash wrapper that
    # execs scripts/model_route.py, which the model-routing acceptance suite
    # runs. A PR changing only the Python module must still select the fabric
    # job that executes it.
    document = _workflow()
    selected = _jobs_for_changed_paths(document, ["scripts/model_route.py"])
    assert "fabric" in selected


def test_model_route_preferences_change_selects_the_fabric_job() -> None:
    document = _workflow()
    selected = _jobs_for_changed_paths(
        document,
        ["scripts/model_route_preferences.py"],
    )
    assert "fabric" in selected


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


def test_setup_action_without_mutation_passes_the_continue_on_error_guard() -> None:
    # Companion to the mutation test above: the real, on-disk composite
    # action must independently satisfy the guard's default code path (no
    # setup_steps override), proving the new coverage is not vacuous.
    _assert_no_continue_on_error(_workflow())


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
    harness_steps = _steps(_job(document, "harness"))
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
        index for index, command in enumerate(setup_commands) if "npm ci --no-audit --no-fund" in command
    )
    assert pin_index < install_index
    assert 'test "$(npm --version)" = "11.12.1"' in setup_commands[pin_index]

    for job_name in ("harness", "fabric", "console", "herdr"):
        steps = _steps(_job(document, job_name))
        uses = [str(step.get("uses", "")) for step in steps]
        checkout_index = next(index for index, action in enumerate(uses) if action.startswith("actions/checkout@"))
        composite_index = uses.index(SETUP_ACTION_USES)
        assert checkout_index < composite_index, job_name

    harness_commands = "\n".join(str(step.get("run", "")) for step in harness_steps)
    for required in (
        "npm run build:types",
        "npm run schema:check:generated",
        "npm run build",
        "git status --porcelain --untracked-files=all -- runtime/agent-fabric-protocol/schemas",
        "scripts/check-harness",
    ):
        assert required in harness_commands
    harness_run_commands = [str(step.get("run", "")).strip() for step in harness_steps if "run" in step]
    build_types_index = harness_run_commands.index("npm run build:types")
    generated_check_index = harness_run_commands.index("npm run schema:check:generated")
    build_index = harness_run_commands.index("npm run build")
    status_index = next(
        index
        for index, command in enumerate(harness_run_commands)
        if "git status --porcelain --untracked-files=all -- runtime/agent-fabric-protocol/schemas" in command
    )
    harness_index = harness_run_commands.index("scripts/check-harness")
    assert build_types_index < generated_check_index < build_index < status_index < harness_index

    fabric_commands = "\n".join(str(step.get("run", "")) for step in fabric_steps)
    for required in (
        "npm run build",
        "npm run test --workspace=@local/agent-fabric-protocol",
        "npm run schema:check --workspace=@local/agent-fabric",
        "npm run typecheck --workspace=@local/agent-fabric",
        "npm run test --workspace=@local/agent-fabric",
        "npm run test:evaluation --workspace=@local/agent-fabric",
        "npm run test:load --workspace=@local/agent-fabric",
        "npm audit --workspace=@local/agent-fabric --omit=dev --audit-level=high",
    ):
        assert required in fabric_commands
    run_steps = [step for step in fabric_steps if "run" in step]
    assert all("working-directory" not in step for step in run_steps)


def test_harness_python_test_dependencies_install_locked_and_cached() -> None:
    # Issue #200: the harness job's Python test tooling (pytest, PyYAML)
    # installs from the committed uv.lock — never from an unconstrained
    # `pip install` — and the setup-uv cache is keyed on that lock so a
    # lock change invalidates the cache. The lock is the pin: pyproject
    # declares the `test` dependency group, uv.lock pins it transitively.
    document = _workflow()
    harness_steps = _steps(_job(document, "harness"))

    setup_uv = next(
        step for step in harness_steps if str(step.get("uses", "")).startswith("astral-sh/setup-uv@")
    )
    options = setup_uv.get("with")
    assert isinstance(options, dict)
    assert options.get("python-version") == "3.12"
    assert options.get("enable-cache") is True
    assert options.get("cache-dependency-glob") == "uv.lock"

    run_commands = [str(step.get("run", "")).strip() for step in harness_steps if "run" in step]
    assert not any("pip install" in command for command in run_commands)
    sync_index = run_commands.index("uv sync --locked --only-group test")
    gate_index = run_commands.index("scripts/check-harness")
    assert sync_index < gate_index

    # The gate runs against the synced environment: scripts/check-harness
    # honours HARNESS_PYTHON before any interpreter fallback.
    gate_step = next(
        step for step in harness_steps if str(step.get("run", "")).strip() == "scripts/check-harness"
    )
    assert gate_step.get("env") == {"HARNESS_PYTHON": "${{ github.workspace }}/.venv/bin/python"}

    pyproject = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))
    # Not a Python package: uv must never build or install the repo itself.
    assert pyproject["tool"]["uv"]["package"] is False
    declared = {
        re.split(r"[<>=~!\[; ]", requirement, maxsplit=1)[0]
        for requirement in pyproject["dependency-groups"]["test"]
    }
    assert declared == {"pytest", "pyyaml"}

    lock_text = (ROOT / "uv.lock").read_text(encoding="utf-8")
    locked = set(re.findall(r'(?m)^name = "([^"]+)"$', lock_text))
    assert {"pytest", "pyyaml"} <= locked


def test_clean_ci_builds_locked_protocol_before_daemon_typecheck() -> None:
    document = _workflow()
    fabric_steps = _steps(_job(document, "fabric"))
    node_setup = next(
        step for step in _setup_action_steps() if str(step.get("uses", "")).startswith("actions/setup-node@")
    )
    cache_paths = str(node_setup.get("with", {}).get("cache-dependency-path", "")).splitlines()
    assert cache_paths == ["package-lock.json"]

    # The clean-protocol assertion must run before the composite installs
    # dependencies so the job proves no stale dist survives checkout.
    dist_clean_index = next(
        index
        for index, step in enumerate(fabric_steps)
        if "test ! -e runtime/agent-fabric-protocol/dist" in str(step.get("run", ""))
    )
    composite_index = next(
        index for index, step in enumerate(fabric_steps) if step.get("uses") == SETUP_ACTION_USES
    )
    assert dist_clean_index < composite_index

    root_package = json.loads(ROOT_PACKAGE.read_text(encoding="utf-8"))
    assert root_package.get("workspaces") == [
        "runtime/agent-fabric-protocol",
        "runtime/agent-fabric",
        "runtime/agent-fabric-herdr",
        "runtime/agent-fabric-console",
    ]
    root_scripts = root_package.get("scripts")
    assert isinstance(root_scripts, dict)
    root_build = root_scripts.get("build")
    assert isinstance(root_build, str)
    assert "tsc -b tsconfig.json" in root_build
    assert "npm run schema:write" in root_build
    schema_write = root_scripts.get("schema:write")
    assert schema_write == "node --import tsx runtime/agent-fabric-protocol/scripts/write-schema.mjs --write"
    generated_schema_check = root_scripts.get("schema:check:generated")
    assert generated_schema_check == (
        "node --import tsx runtime/agent-fabric-protocol/scripts/write-schema.mjs --check"
    )
    protocol_package = json.loads(PROTOCOL_PACKAGE.read_text(encoding="utf-8"))
    assert protocol_package["scripts"]["build"] == (
        "tsc -p tsconfig.build.json && node --import tsx scripts/write-schema.mjs --write"
    )
    schema_writer = PROTOCOL_SCHEMA_WRITER.read_text(encoding="utf-8")
    assert 'from "../dist/' not in schema_writer
    for source_module in ("schema", "mcp-projection", "operations"):
        assert f'from "../src/{source_module}.ts"' in schema_writer
    assert ROOT_LOCK.is_file()
    assert not list((ROOT / "runtime").glob("*/package-lock.json"))
    root_dev_dependencies = root_package.get("devDependencies")
    assert isinstance(root_dev_dependencies, dict)
    assert root_dev_dependencies.get("tsx") == "4.23.1"
    # Subscription provider CLIs are host tools admitted by identity and
    # interface, not repository dependencies or version locks.
    assert root_package.get("dependencies") is None
    assert root_scripts.get("compatibility:check:primary") == (
        "tsx runtime/agent-fabric/scripts/verify-primary-compatibility.ts"
    )

    package = json.loads(FABRIC_PACKAGE.read_text(encoding="utf-8"))
    scripts = package.get("scripts")
    assert isinstance(scripts, dict)
    assert scripts.get("prepack") == "npm run build --include-workspace-root"
    daemon_check = scripts.get("check")
    assert "check:protocol" not in scripts
    assert isinstance(daemon_check, str)
    assert "npm --prefix" not in daemon_check

    for relative in (
        "runtime/agent-fabric/package.json",
        "runtime/agent-fabric-herdr/package.json",
        "runtime/agent-fabric-console/package.json",
    ):
        consumer = json.loads((ROOT / relative).read_text(encoding="utf-8"))
        consumer_scripts = consumer.get("scripts")
        assert isinstance(consumer_scripts, dict)
        assert consumer_scripts.get("precheck") == "npm run build --include-workspace-root"

    fabric_commands = "\n".join(str(step.get("run", "")) for step in fabric_steps)
    assert "test ! -e runtime/agent-fabric-protocol/dist" in fabric_commands
    assert (
        fabric_commands.index("npm run build")
        < fabric_commands.index("npm run test --workspace=@local/agent-fabric-protocol")
        < fabric_commands.index("npm run typecheck --workspace=@local/agent-fabric")
    )
    assert "test -f runtime/agent-fabric-protocol/dist/index.d.ts" in fabric_commands


def test_ci_runs_console_and_herdr_product_gates() -> None:
    document = _workflow()
    expected = {
        "console": {
            "commands": {
                "npm run build",
                "npm run typecheck --workspace=@local/agent-fabric-console",
                "npm run test --workspace=@local/agent-fabric-console",
                "npm run test:evaluation --workspace=@local/agent-fabric-console",
                "npm run test:load --workspace=@local/agent-fabric-console",
                "npm audit --workspace=@local/agent-fabric-console --omit=dev --audit-level=high",
            },
        },
        "herdr": {
            "commands": {
                "npm run build",
                "npm run typecheck --workspace=@local/agent-fabric-herdr",
                "npm run test --workspace=@local/agent-fabric-herdr",
                "npm audit --workspace=@local/agent-fabric-herdr --omit=dev --audit-level=high",
            },
        },
    }
    for job_name, contract in expected.items():
        steps = _steps(_job(document, job_name))
        assert any(step.get("uses") == SETUP_ACTION_USES for step in steps)
        run_steps = [step for step in steps if "run" in step]
        assert all("working-directory" not in step for step in run_steps)
        commands = "\n".join(str(step.get("run", "")) for step in run_steps)
        assert all(command in commands for command in contract["commands"])


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
    assert len(npm_updates) == 1
    assert npm_updates[0].get("directory") == "/"
    assert any(item.get("package-ecosystem") == "github-actions" and item.get("directory") == "/" for item in updates)

    template = (ROOT / ".github" / "pull_request_template.md").read_text(encoding="utf-8").lower()
    for heading in (
        "## summary",
        "## decision requested",
        "## risk and rollback",
        "## evidence",
        "## independent review",
    ):
        assert heading in template

    # The evidence table replaces attestation checkboxes with externally
    # verifiable rows bound to the exact head.
    assert "| gate | command or artifact | result | head sha | n/a reason |" in template
    assert "- [ ]" not in template

    for evidence in (
        "direct cutover",
        "no legacy reader",
        "compatibility bridge",
        "migration preflight",
        "rollback or forward-repair",
        "trigger or query-plan evidence",
    ):
        assert evidence in template
    assert "historical formats remain readable" not in template

    for evidence in (
        "base:",
        "head under review",
        "reviewer role",
        "model family",
        "exact head reviewed",
        "stays open after merge",
        "later commit invalidates",
        "mermaid",
    ):
        assert evidence in template


def test_dependabot_automerge_excludes_pinned_primary_provider_packages() -> None:
    # Issues #195 and #208: these packages participate in the exact executable
    # and protocol closure, so even patch updates require compatibility review.
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


def test_github_work_item_and_runbook_cover_the_intake_contract() -> None:
    form = yaml.safe_load(
        (ROOT / ".github" / "ISSUE_TEMPLATE" / "work-item.yml").read_text(
            encoding="utf-8"
        )
    )
    assert {item.get("id") for item in form["body"] if "id" in item} >= {
        "problem-evidence",
        "outcome",
        "scope",
        "acceptance",
        "dependencies",
        "risk-authority-gates",
    }

    runbook = (ROOT / "docs" / "runbooks" / "github-workflow.md").read_text(
        encoding="utf-8"
    ).lower()
    for status in (
        "backlog",
        "ready",
        "in progress",
        "in review",
        "awaiting user",
        "done",
    ):
        assert status in runbook
    for outcome in ("accepted", "rejected", "deferred", "duplicate"):
        assert outcome in runbook
    assert "`closes #n`" in runbook
    assert "`references #n`" in runbook

    maintaining = (ROOT / "MAINTAINING.md").read_text(encoding="utf-8")
    assert maintaining.count("(docs/runbooks/github-workflow.md)") == 1


def test_live_workspace_guides_use_only_the_root_install_and_build_graph() -> None:
    for guide in WORKSPACE_GUIDES:
        source = guide.read_text(encoding="utf-8")
        assert "npm --prefix" not in source, guide
        assert not re.search(r"(?m)^npm install(?:\s|$)", source), guide

    operations = WORKSPACE_GUIDES[0].read_text(encoding="utf-8")
    for command in (
        "npm ci --no-audit --no-fund",
        "npm run build",
        "npm run check",
    ):
        assert command in operations
