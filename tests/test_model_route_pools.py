"""Global weighted route pools and their four selection modes (#848)."""

from collections import Counter
from datetime import datetime, timedelta, timezone
import importlib.util
import json
import os
from pathlib import Path
import random
import subprocess
import sys

import pytest


from tests.support.front_door import front_door_binary

ROOT = Path(__file__).resolve().parents[1]
SEED = json.loads((ROOT / "config" / "model-routing.json").read_text())
# The selection tests pin their own senior and design pools, so reweighting the seed never moves their arithmetic.
CATALOG = {**SEED, "routes": {**SEED["routes"], "senior": [
    {"model": "claude/claude-opus-5-5", "weight": "high"},
    {"model": "codex/gpt-6.1-sol", "weight": "normal", "effort": ["high", "xhigh"]},
    {"model": "codex/gpt-6-astra", "weight": "sparing"}], "design": [
    {"model": "claude/claude-sonnet-5-5", "weight": "high"},
    {"model": "claude/claude-opus-5-5", "weight": "normal"},
    {"model": "opencode/opencode-go/deepseek-v4.1-flash", "weight": "sparing"},
    {"model": "opencode/opencode-go/glm-5.3-flash", "weight": "sparing"},
    {"model": "opencode/opencode/mimo-v2.6-flash-free", "weight": "sparing"},
    {"model": "codex/gpt-6-astra", "weight": "sparing"}]}}


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


router = load("pools_router_under_test", ROOT / "scripts" / "model_route.py")
pools = router._pools
for adapter_entry in SEED["adapters"].values():  # the loader's derivation, since these tests read the raw file
    router._apply_free_pattern(adapter_entry)


def availability(catalog=CATALOG, installed=lambda adapter: True, cooling=()):
    until = (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat()
    checked = pools.Availability(router, catalog, installed=installed,
                                 cooldowns={key: {"cooling_until": until} for key in cooling})
    checked._enabled = {adapter: "" for adapter in catalog["adapters"]}  # skip per-test adapter YAML parsing
    return checked


def pick(request, tmp_path, catalog=CATALOG, seed=0, **options):
    return pools.pick(router, catalog, {"project": "/work/a", **request},
                      availability=options.pop("available", None) or availability(catalog, **options),
                      state_path=tmp_path / "route-rotation.json", effort_order=router.EFFORT_ORDER,
                      rng=random.Random(seed))


def models(result):
    return [f"{item['adapter']}/{item['model']}" for item in result["picks"]]


def test_seed_routes_name_registered_models_with_the_tiny_weight_vocabulary():
    CATALOG = SEED
    assert set(CATALOG["routes"]) == {"principal", "senior", "junior", "utility", "design", "writing"}
    for name, entries in CATALOG["routes"].items():
        for entry in entries:
            assert entry["weight"] in {"high", "normal", "sparing"}
            adapter, model = pools.split_model(router, CATALOG, entry["model"])
            registered, _ = router.model_route_live._registered_match(adapter, model, CATALOG)
            assert registered is not None, f"{name}: {entry['model']}"
            for effort in entry.get("effort", []):
                assert effort in registered["efforts"]
    assert all(target in CATALOG["routes"] for target in CATALOG["route_synonyms"].values())
    assert set(CATALOG["task_class_routes"]) - {"review", "bulk"} <= set(CATALOG["route_synonyms"]) | set(CATALOG["routes"])


def test_default_pools_hold_only_free_models_with_explicit_no_training():
    for name, entries in SEED["routes"].items():
        for entry in entries:
            adapter, model = pools.split_model(router, SEED, entry["model"])
            if "free" in pools.model_traits(router, SEED, adapter, model):
                registered, _ = router.model_route_live._registered_match(adapter, model, SEED)
                assert registered.get("trains_on_prompts") is False, f"{name}: {entry['model']}"


def test_top_is_the_highest_weight_available_and_deterministic(tmp_path):
    first = pick({"route": "senior"}, tmp_path, seed=1)
    assert models(first) == ["claude/claude-opus-5-5"]
    assert first["picks"][0]["reason"] == "senior top"
    assert models(pick({"route": "senior"}, tmp_path, seed=99)) == models(first)
    # The seed pools are the three tiers: deciders, mid-level judges and tier-1 lenses.
    assert models(pick({"route": "principal"}, tmp_path, catalog=SEED)) == ["claude/claude-opus-5-5"]
    assert models(pick({"route": "senior"}, tmp_path, catalog=SEED)) == ["claude/claude-sonnet-5-5"]
    assert models(pick({"route": "junior"}, tmp_path, catalog=SEED)) == ["claude/claude-haiku-5-5"]
    bulk = {entry["model"]: entry["weight"] for entry in SEED["routes"]["junior"]}
    assert {model for model, weight in bulk.items() if weight == "high"} == {"claude/claude-haiku-5-5",
                                                                            "codex/gpt-6-luna"}
    cooling = []
    for _ in bulk:
        cooling.extend(models(pick({"route": "junior"}, tmp_path, catalog=SEED, cooling=cooling)))
    # Free routes are rate limited, so they stay sparing and follow the subscription routes.
    rank = {"high": 0, "normal": 1, "sparing": 2}
    assert cooling == sorted(bulk, key=lambda model: rank[bulk[model]])
    assert not set(bulk) & {"claude/claude-sonnet-5-5", "codex/gpt-6.1-sol", "claude/claude-opus-5-5"}


def test_profile_pool_mode_rotates_when_request_does_not_choose_a_mode(tmp_path, monkeypatch):
    monkeypatch.setenv("PROVENANT_KNOB_ROUTING_POOL_MODE", "rotate")
    first = pick({"route": "senior"}, tmp_path)
    second = pick({"route": "senior"}, tmp_path)
    assert first["mode"] == second["mode"] == "rotate"
    assert models(first) != models(second)


def test_economy_profile_rotates_without_a_pool_mode_knob(tmp_path, monkeypatch):
    monkeypatch.delenv("PROVENANT_KNOB_ROUTING_POOL_MODE", raising=False)
    monkeypatch.setenv("PROVENANT_PROFILE", "economy")
    result = pick({"route": "senior"}, tmp_path)
    assert result["mode"] == "rotate"


def test_every_mode_filters_cooling_uninstalled_and_disabled_models(tmp_path):
    cooling = pick({"route": "senior"}, tmp_path, cooling=("claude/claude-opus-5-5",))
    assert models(cooling) == ["codex/gpt-6.1-sol"]
    assert cooling["picks"][0]["effort"] == "high"
    assert any("claude-opus-5-5: cooling until" in warning for warning in cooling["warnings"])
    missing = pick({"route": "senior", "council": 3}, tmp_path, installed=lambda adapter: adapter != "claude")
    assert "claude/claude-opus-5-5" not in models(missing)
    assert any("not installed" in warning for warning in missing["warnings"])
    with pytest.raises(pools.PoolError) as error:
        pick({"route": "writing"}, tmp_path, installed=lambda adapter: False)
    assert error.value.code == "route_unavailable"
    disabled = availability()
    disabled._enabled["claude"] = "disabled"
    assert models(pick({"route": "senior"}, tmp_path, available=disabled)) == ["codex/gpt-6.1-sol"]


def test_route_effort_band_clamps_the_callers_effort(tmp_path):
    cooling = ("claude/claude-opus-5-5",)
    assert pick({"route": "senior", "effort": "low"}, tmp_path, cooling=cooling)["picks"][0]["effort"] == "high"
    assert pick({"route": "senior", "effort": "xhigh"}, tmp_path, cooling=cooling)["picks"][0]["effort"] == "xhigh"
    assert pick({"route": "senior", "effort": "medium"}, tmp_path)["picks"][0]["effort"] == "medium"
    raised = pick({"route": "senior", "effort": "low"}, tmp_path, cooling=cooling)
    assert "effort low raised to high by the senior band for codex/gpt-6.1-sol" in raised["warnings"]
    lowered = pick({"route": "senior", "effort": "ultra"}, tmp_path, cooling=cooling)
    assert "effort ultra lowered to xhigh by the senior band for codex/gpt-6.1-sol" in lowered["warnings"]
    with pytest.raises(pools.PoolError) as error:
        pick({"route": "senior", "effort": "max"}, tmp_path)
    assert error.value.code == "effort_invalid" and "xhigh" in error.value.fix


def test_council_spreads_families_first_and_names_each_member(tmp_path):
    for seed in range(40):
        result = pick({"route": "design", "council": 3}, tmp_path, seed=seed)
        families = [item["family"] for item in result["picks"]]
        assert len(set(families)) == 3, (seed, families)
        assert [item["reason"] for item in result["picks"]] == [
            "design council 1/3", "design council 2/3", "design council 3/3"]
        senior = pick({"route": "senior", "council": 2}, tmp_path, seed=seed)
        assert {item["family"] for item in senior["picks"]} == {"anthropic", "openai"}


def test_council_weights_are_sampling_odds_and_sparing_is_rare(tmp_path):
    drawn = Counter(models(pick({"route": "senior", "council": 1}, tmp_path, seed=seed))[0] for seed in range(400))
    assert drawn["claude/claude-opus-5-5"] > drawn["codex/gpt-6.1-sol"] > drawn["codex/gpt-6-astra"] > 0
    assert drawn["codex/gpt-6-astra"] < 400 / 8


def test_a_prompt_training_model_never_takes_a_council_seat_for_its_family(tmp_path):
    argon = "opencode/opencode-go/gemini-4-argon-contributor"
    assert {"model": argon, "weight": "sparing"} in SEED["routes"]["principal"]
    drawn = Counter(model for seed in range(200)
                    for model in models(pick({"route": "principal", "council": 2}, tmp_path, catalog=SEED, seed=seed)))
    # As the lone Google family Argon would sit on every two-member council; by odds it sits on about one in thirteen.
    assert 0 < drawn[argon] < 200 / 6


def test_council_larger_than_the_pool_repeats_with_a_warning(tmp_path):
    result = pick({"route": "senior", "council": 5}, tmp_path)
    assert len(result["picks"]) == 5
    assert any("exceeds 3 available" in warning for warning in result["warnings"])


def test_confidential_skips_free_training_models_and_otherwise_warns(tmp_path):
    free = "opencode/opencode/mimo-v2.6-flash-free"
    open_council = [pick({"route": "design", "council": 8}, tmp_path, seed=seed) for seed in range(20)]
    chosen = [result for result in open_council if free in models(result)]
    assert chosen, "free models stay in an ordinary pool"
    assert any("may train on prompts" in warning and "confidential" in warning for warning in chosen[0]["warnings"])
    for seed in range(20):
        private = pick({"route": "design", "council": 3, "confidential": True}, tmp_path, seed=seed)
        assert not any(model.endswith("-free") for model in models(private))
        assert any(f"skipped {free}: trains on prompts" in warning for warning in private["warnings"])
    with pytest.raises(pools.PoolError) as error:
        pick({"models": ["opencode/mimo-free"], "confidential": True}, tmp_path)
    assert error.value.code == "route_unavailable"


def test_rotate_cycles_by_weight_with_a_per_project_cursor(tmp_path):
    sequence = [models(pick({"route": "senior", "rotate": True}, tmp_path))[0] for _ in range(13)]
    assert Counter(sequence) == {"claude/claude-opus-5-5": 8, "codex/gpt-6.1-sol": 4, "codex/gpt-6-astra": 1}
    assert len(set(sequence[:2])) == 2, "rotation mixes models from the second call"
    assert pick({"route": "senior", "rotate": True}, tmp_path)["picks"][0]["reason"] == "senior rotate"
    other = [models(pick({"route": "senior", "rotate": True, "project": "/work/b"}, tmp_path))[0] for _ in range(2)]
    assert other == sequence[:2]
    state = json.loads((tmp_path / "route-rotation.json").read_text())
    assert set(state["projects"]) == {"/work/a", "/work/b"}


def test_explicit_models_bypass_routes_and_keep_their_effort(tmp_path):
    result = pick({"models": ["codex/gpt-6-luna@low", "claude/claude-haiku-4-5"]}, tmp_path)
    assert models(result) == ["codex/gpt-6-luna", "claude/claude-haiku-4-5"]
    assert result["picks"][0]["effort"] == "low" and "effort" not in result["picks"][1]
    assert [item["reason"] for item in result["picks"]] == ["ad-hoc council 1/2", "ad-hoc council 2/2"]
    assert models(pick({"models": ["opencode/mimo-v2.6-flash-free"]}, tmp_path)) == [
        "opencode/opencode/mimo-v2.6-flash-free"]
    mixed = pick({"models": ["codex/gpt-6-luna"], "route": "bulk", "adapter": "claude"}, tmp_path)
    assert models(mixed) == ["codex/gpt-6-luna"]
    assert any(warning.startswith("route bulk ignored: models bypasses routes") for warning in mixed["warnings"])
    assert any(warning.startswith("adapter claude ignored") for warning in mixed["warnings"])


def test_models_rejects_only_an_unknown_adapter_and_passes_unregistered_ids_with_a_warning(tmp_path):
    with pytest.raises(pools.PoolError) as error:
        pick({"models": ["nosuch/gpt-6-luna"]}, tmp_path)
    assert error.value.code == "models_invalid"
    assert "codex" in error.value.fix and "claude" in error.value.fix
    passed = pick({"models": ["codex/gpt-6-nova"]}, tmp_path)
    assert models(passed) == ["codex/gpt-6-nova"]
    assert any("not in the catalogue" in warning for warning in passed["warnings"])


def test_precedence_warns_instead_of_rejecting(tmp_path):
    both = pick({"route": "senior", "council": 2, "rotate": True}, tmp_path)
    assert len(both["picks"]) == 2
    assert any(warning.startswith("rotate ignored") for warning in both["warnings"])
    with pytest.raises(pools.PoolError) as error:
        pick({"rotate": True}, tmp_path)
    assert error.value.code == "route_required" and "senior" in error.value.fix


def test_spelling_aliases_collapse_so_an_overlay_switches_the_seed_off(tmp_path):
    catalog = json.loads(json.dumps(CATALOG))
    catalog["routes"]["senior"] = [{"model": "codex/gpt-6-sol", "weight": "off"}, *catalog["routes"]["senior"]]
    entries = pools.pool(router, catalog, "senior", router.EFFORT_ORDER)
    canonical = [entry for entry in entries if entry["key"] == "codex/gpt-6.1-sol"]
    assert len(canonical) == 1 and canonical[0]["weight"] == 0
    assert models(pick({"route": "senior", "adapter": "codex"}, tmp_path, catalog=catalog)) == ["codex/gpt-6-astra"]


def test_availability_skips_entries_the_adapter_cannot_run(tmp_path):
    catalog = json.loads(json.dumps(CATALOG))
    catalog["routes"]["bulk"] = [{"model": "agy/gpt-6.1-sol", "weight": "high"}, {"model": "codex/gpt-6-luna"}]
    checked = pools.Availability(router, catalog, installed=lambda adapter: True, cooldowns={})
    result = pick({"route": "bulk"}, tmp_path, catalog=catalog, available=checked)
    assert models(result) == ["codex/gpt-6-luna"]
    assert any("agy/gpt-6.1-sol" in warning and "cannot run the openai family" in warning
               for warning in result["warnings"])


def test_an_adapter_drawing_on_credits_runs_only_when_nothing_with_allowance_qualifies(tmp_path):
    def checked(cooling=(), last_resort=True):
        value = availability(cooling=cooling)
        value._quota = {"codex": {"state": "credit_only", **({"last_resort": True} if last_resort else {})}}
        return value
    result = pick({"route": "senior", "council": 2}, tmp_path, available=checked())
    assert set(models(result)) == {"claude/claude-opus-5-5"}
    assert any("codex/gpt-6.1-sol: drawing on credits" in warning for warning in result["warnings"])
    # With Claude cooling, Codex's credits are the last resort; naming the adapter pins Codex outright.
    result = pick({"route": "senior"}, tmp_path, available=checked(cooling=("claude/*",)))
    assert models(result) == ["codex/gpt-6.1-sol"]
    assert any("draws on credits: nothing with allowance" in warning for warning in result["warnings"])
    result = pick({"route": "senior", "adapter": "codex"}, tmp_path, available=checked())
    assert models(result) == ["codex/gpt-6.1-sol"] and not any("credits" in warning for warning in result["warnings"])
    assert models(pick({"models": ["codex/gpt-6-astra"]}, tmp_path, available=checked())) == ["codex/gpt-6-astra"]
    # The `normal` policy marks no last resort, so Codex competes on weight.
    result = pick({"route": "senior", "council": 3}, tmp_path, available=checked(last_resort=False))
    assert "codex/gpt-6.1-sol" in models(result)


def test_task_classes_and_synonyms_map_onto_routes(tmp_path):
    assert pick({"route": "second-opinion"}, tmp_path, catalog=SEED)["route"] == "senior"
    assert pick({"route": "critical-review"}, tmp_path, catalog=SEED)["route"] == "principal"
    assert pick({"route": "ui-taste"}, tmp_path, catalog=SEED)["route"] == "design"
    for lens in ("junior", "mechanical", "lens", "research"):
        assert pick({"route": lens}, tmp_path, catalog=SEED)["route"] == "junior"
    assert pick({"route": "implementation"}, tmp_path, catalog=SEED)["route"] == "senior", "matches its task class"
    for name in ("senior", "principal", "junior"):
        assert pick({"route": name}, tmp_path, catalog=SEED)["route"] == name
    with pytest.raises(pools.PoolError) as error:
        pick({"route": "fastest"}, tmp_path)
    assert error.value.code == "route_invalid"


@pytest.mark.parametrize("old,new", [
    ("scout", "junior"), ("bulk", "junior"), ("workhorse", "senior"), ("strong", "senior"),
    ("review", "senior"), ("Flagship", "principal"),
])
def test_retired_tier_names_are_rejected_with_their_replacement(tmp_path, old, new):
    with pytest.raises(pools.PoolError) as error:
        pick({"route": old}, tmp_path, catalog=SEED)
    assert error.value.code == "route_renamed"
    assert f"route '{old}' was renamed to '{new}'" in error.value.fix


def test_adapter_narrows_the_pool(tmp_path):
    assert models(pick({"route": "senior", "adapter": "codex"}, tmp_path)) == ["codex/gpt-6.1-sol"]
    with pytest.raises(pools.PoolError) as error:
        pick({"route": "senior", "adapter": "opencode"}, tmp_path)
    assert error.value.code == "route_adapter_empty"
    containing = [name for name, entries in CATALOG["routes"].items()
                  if any(entry["model"].startswith("opencode/") for entry in entries)]
    assert containing
    assert all(name in str(error.value) for name in containing)
    assert "model" in str(error.value)


def overlay_environment(tmp_path, overlay):
    instance = tmp_path / "instance"
    (instance / "config").mkdir(parents=True)
    (instance / "config" / "model-routing.json").write_text(json.dumps(overlay))
    return {**os.environ, "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT), "AGENT_FABRIC_INSTANCE_ROOT": str(instance),
            "AGENT_FABRIC_STATE_ROOT": str(tmp_path / "state"),
            "CODEX_HOME": str(Path.home() / ".codex"),
            "FABRIC_COOLDOWNS_PATH": str(tmp_path / "state/cooldowns.json")}


def model_route(env, *args, stdin=None):
    return subprocess.run([sys.executable, str(ROOT / "scripts" / "model_route.py"), *args],
                          input=stdin, text=True, capture_output=True, env=env, check=False)


def test_instance_overlay_reorders_and_reweights_a_pool(tmp_path):
    env = overlay_environment(tmp_path, {"routes": {
        "senior": [{"model": "codex/gpt-6-astra", "weight": "high"},
                   {"model": "claude/claude-opus-5-5", "weight": "off"}],
        "local": [{"model": "codex/gpt-6-luna"}],
        "broken": [{"model": "no-adapter", "weight": "loud"}],
    }, "route_synonyms": {"deep": "senior", "principal": "senior"}})
    run = model_route(env, "routes", "--json")
    assert run.returncode == 0, run.stderr
    document = json.loads(run.stdout)
    folded = document["routes"]["senior"]
    assert [entry["model"] for entry in folded][:2] == ["codex/gpt-6-astra", "claude/claude-opus-5-5"]
    assert folded[1]["weight"] == "off" and folded[1]["availability"] == "off"
    assert [entry["model"] for entry in folded][2:] == [entry["model"] for entry in SEED["routes"]["senior"] if entry["model"] not in {"codex/gpt-6-astra", "claude/claude-opus-5-5"}], \
        "the tier keeps its seed fallbacks"
    assert document["routes"]["principal"][0]["model"] == SEED["routes"]["principal"][0]["model"], \
        "a synonym cannot shadow a pool"
    second = tmp_path / "second"
    second.mkdir()
    senior = model_route(overlay_environment(second, {"routes": {
        "senior": [{"model": "codex/gpt-6-astra", "weight": "high"}]}}), "routes", "--json")
    merged = json.loads(senior.stdout)["routes"]["senior"]
    assert [entry["model"] for entry in merged] == [
        "codex/gpt-6-astra", *(entry["model"] for entry in SEED["routes"]["senior"] if entry["model"] != "codex/gpt-6-astra")]
    sol = next(entry for entry in merged if entry["model"] == "codex/gpt-6.1-sol")
    assert sol["effort"] == "high-xhigh", "unlisted entries keep their fields"
    assert document["routes"]["local"][0]["weight"] == "normal"
    assert "broken" not in document["routes"]
    request = json.dumps({"requests": [{"route": "deep"}, {"route": "local", "adapter": "codex"}]})
    picked = model_route(env, "pick", stdin=request)
    assert picked.returncode == 0, picked.stderr
    results = json.loads(picked.stdout)["results"]
    if results[0]["status"] == "ok":  # needs an installed codex CLI
        assert results[0]["picks"][0]["model"] == "gpt-6-astra"
    snapshot = json.loads(model_route(env, "snapshot", "--json").stdout)
    assert any(note.startswith("routes.broken") for note in snapshot["drift"])


@pytest.mark.parametrize("overlay,message", [
    ({"routes": {"strong": [{"model": "codex/gpt-6.1-sol"}]}}, "route 'strong' was renamed to 'senior'"),
    ({"routes": {"scout": [{"model": "codex/gpt-6-luna"}]}}, "route 'scout' was renamed to 'junior'"),
    ({"route_synonyms": {"deep": "flagship"}}, "route_synonyms target 'flagship' was renamed to 'principal'"),
    ({"task_class_routes": {"legwork": {"alias": "workhorse"}}}, "alias 'workhorse' was renamed to 'senior'"),
    ({"families": {"openai": {"aliases": {"flagship": ["gpt-6-astra"]}}}}, "alias 'flagship' was renamed to 'principal'"),
    ({"adapters": {"opencode": {"aliases": {"scout": ["x"]}}}}, "alias 'scout' was renamed to 'junior'"),
    ({"families": {"openai": {"risk_tier_overrides": {"crucial": {"alias": "flagship"}}}}},
     "alias 'flagship' was renamed to 'principal'"),
])
def test_overlay_with_a_retired_tier_name_fails_fast(overlay, message):
    with pytest.raises(pools.RenamedNameError, match=message):
        pools.reject_renamed(overlay)


def test_routes_command_rejects_retired_tier_in_overlay(tmp_path):
    env = overlay_environment(tmp_path, {"routes": {
        "strong": [{"model": "codex/gpt-6.1-sol"}],
    }})
    run = model_route(env, "routes")
    assert run.returncode == 2
    assert "model-route:" in run.stderr
    assert "overlay" in run.stderr


def test_overlay_may_name_adapter_aliases_by_tier(tmp_path):
    overlay = {"task_class_routes": {"implementation": {"alias": "senior", "effort": "high"}},
               "families": {"openai": {"aliases": {"principal": ["gpt-6-astra"], "senior": ["gpt-6.1-sol"]}}},
               "adapters": {"opencode": {"aliases": {"junior": ["opencode/glm-5.3-flash-free"]}}}}
    assert pools.reject_renamed(overlay) == overlay


def test_pick_command_answers_every_request_and_types_rejections(tmp_path):
    env = overlay_environment(tmp_path, {})
    run = model_route(env, "pick", "--seed", "3", stdin=json.dumps({"requests": [
        {"route": "nope"}, {"route": "senior", "council": 9}, "bad",
        {"models": ["codex/gpt-6-luna@warp"]},
    ]}))
    assert run.returncode == 0, run.stderr
    assert [result["error"] for result in json.loads(run.stdout)["results"]] == [
        "route_invalid", "council_invalid", "invalid_input", "effort_invalid"]


def test_routes_prints_each_pool_with_live_availability(tmp_path):
    env = overlay_environment(tmp_path, {"routes": {"junior": [
        *SEED["routes"]["junior"],
        {"model": "opencode/opencode/mimo-v2.6-flash-free", "weight": "sparing"},
    ]}})
    env["PATH"] = str(tmp_path / "empty-bin")
    state = tmp_path / "state"
    state.mkdir()
    until = (datetime.now(timezone.utc) + timedelta(hours=1)).isoformat()
    (state / "cooldowns.json").write_text(json.dumps({"cooldowns": {"codex/gpt-6-luna": {"cooling_until": until}}}))
    run = model_route(env, "routes")
    assert run.returncode == 0, run.stderr
    for name in ("principal", "senior", "junior", "design", "writing"):
        assert f"\n{name}\n" in f"\n{run.stdout}"
    assert "unavailable or cooling" in run.stdout
    assert "use --all" in run.stdout
    all_run = model_route(env, "routes", "--all")
    assert "not installed" in all_run.stdout
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    (bin_dir / "codex").write_text("#!/bin/sh\n")
    (bin_dir / "codex").chmod(0o755)
    env["PATH"] = str(bin_dir)
    lines = model_route(env, "routes", "--all").stdout.splitlines()
    assert any("codex/gpt-6-luna" in line and "cooling until" in line for line in lines)
    assert any("codex/gpt-6-astra" in line and line.rstrip().endswith("available") for line in lines)
    assert "[free, trains-on-prompts]" in all_run.stdout


def test_route_line_names_why_the_pool_picked_the_model():
    dispatch_run = load("pools_dispatch_run_under_test", ROOT / "skills/orchestrate/scripts/dispatch_run.py")
    line = "Route: opencode/deepseek-v4.1-flash (deepseek; observed)"
    assert dispatch_run.with_pick_reason(line, "design council 2/3") == (
        "Route: opencode/deepseek-v4.1-flash (deepseek; observed; design council 2/3)")
    assert dispatch_run.with_pick_reason(line, None) == line
    once = dispatch_run.with_pick_reason(line, "design council 2/3")
    assert dispatch_run.with_pick_reason(once, "design council 2/3") == once
    args = dispatch_run.parser().parse_args([
        "--run-dir", "/tmp/run", "--prompt-file", "p.md", "--adapter", "codex", "--model", "gpt-6-luna",
        "--pick-reason", "bulk top"])
    args.task_id, args.access_mode, args.worktree = "task-1", "read_only", None
    row = dispatch_run.contract_row(args, Path("/tmp/run"), 1, Path("/tmp/run/a"),
                                    {"model": "gpt-6-luna", "route": {"model_family": "openai"}}, "now")
    assert row["provenance"]["line"] == "Route: codex/gpt-6-luna (openai; resolved; bulk top)"
    assert row["provenance"]["pick_reason"] == "bulk top"
    batch_run = load("pools_batch_run_under_test", ROOT / "skills/orchestrate/scripts/batch_run.py")
    command = batch_run._command({"id": "c-1", "adapter": "codex", "prompt_file": "p.md", "role": "worker",
                                  "timeout": 60, "model": "gpt-6-luna", "pick_reason": "ad-hoc council 1/2"},
                                 Path("/tmp/run"))
    assert command[command.index("--pick-reason") + 1] == "ad-hoc council 1/2"





def exec_routing_module():
    return importlib.import_module("skills.orchestrate.scripts.exec_routing")


def test_confidential_filters_every_fallback_even_under_fallback_any():
    exec_routing = exec_routing_module()
    plan = {"adapter": "codex", "model": "gpt-6.1-sol", "route": {"fallback_candidates": [
        {"adapter": "opencode", "model": "opencode/mimo-v2.6-flash-free"},
        {"adapter": "opencode", "model": "opencode-go/deepseek-v4.1-flash", "trains_on_prompts": True},
        {"adapter": "codex", "model": "gpt-6-luna"}]}}
    catalogue = {"adapters": {}, "models": {}}
    everything = exec_routing.candidates(plan, "any", catalogue)
    assert len(everything) == 3, "fallback any keeps free and training routes for ordinary work"
    private = exec_routing.candidates(plan, "any", catalogue, confidential=True)
    assert [item["model"] for item in private] == ["gpt-6-luna"]
    assert exec_routing.disclosure_risk("codex", "gpt-6-luna", {}) is None
    assert exec_routing.disclosure_risk("opencode", "x", {"plan_cap_usd": 0})


def test_confidential_refuses_a_resolved_route_that_trains_and_names_the_fix():
    dispatch_run = load("pools_dispatch_confidential_under_test", ROOT / "skills/orchestrate/scripts/dispatch_run.py")
    refusal = dispatch_run.dispatch_preflight.confidential_refusal("opencode", "opencode/mimo-v2.6-flash-free", {})
    assert refusal.startswith("opencode/opencode/mimo-v2.6-flash-free is a free tier")
    assert "fix:" in refusal
    assert dispatch_run.dispatch_preflight.confidential_refusal("codex", "gpt-6-luna", {"trains_on_prompts": False}) is None
    args = dispatch_run.parser().parse_args(["--run-dir", "/tmp/run", "--prompt-file", "p.md", "--adapter", "codex",
                                             "--confidential"])
    assert args.confidential is True
    batch_run = load("pools_batch_confidential_under_test", ROOT / "skills/orchestrate/scripts/batch_run.py")
    command = batch_run._command({"id": "c", "adapter": "codex", "prompt_file": "p.md", "role": "worker",
                                  "timeout": 60, "model": "gpt-6-luna", "confidential": True}, Path("/tmp/run"))
    assert "--confidential" in command
    args.task_id, args.access_mode, args.worktree = "task-1", "read_only", None
    args.model = "gpt-6-luna"
    row = dispatch_run.contract_row(args, Path("/tmp/run"), 1, Path("/tmp/run/a"),
                                    {"model": "gpt-6-luna", "route": {"model_family": "openai"}}, "now")
    assert row["provenance"]["requested"]["confidential"] is True, "resume and handoff read it back"


MIXED_SCOUT = {"adapters": {"opencode": {"aliases": {"junior": [
    "opencode/mimo-v2.6-flash-free", "opencode-go/deepseek-v4.1-flash"]}}}}


def test_confidential_alias_steps_to_its_first_safe_member():
    exec_routing = exec_routing_module()
    catalog = json.loads(json.dumps(CATALOG))
    catalog["adapters"]["opencode"]["aliases"]["junior"] = MIXED_SCOUT["adapters"]["opencode"]["aliases"]["junior"]
    assert exec_routing.confidential_alternatives(
        "opencode", "opencode/mimo-v2.6-flash-free", "junior", catalog) == ["opencode-go/deepseek-v4.1-flash"]
    assert exec_routing.confidential_alternatives("opencode", "x", "nosuch", catalog) == []


def test_confidential_preflight_uses_a_mixed_alias_and_refuses_only_when_nothing_safe_remains(tmp_path, monkeypatch):
    instance = tmp_path / "instance"
    (instance / "config").mkdir(parents=True)
    (instance / "config" / "model-routing.json").write_text(json.dumps(MIXED_SCOUT))
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(instance))
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path / "state"))
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))
    monkeypatch.chdir(tmp_path)
    dispatch_run = load("pools_dispatch_mixed_alias_under_test", ROOT / "skills/orchestrate/scripts/dispatch_run.py")
    task = {"id": "t", "adapter": "opencode", "alias": "junior", "prompt": "hi"}
    ordinary = dispatch_run.dispatch_preflight.preflight_tasks([task], tmp_path)
    assert ordinary.get("routes", [{}])[0].get("resolved_model") == "opencode/mimo-v2.6-flash-free", ordinary
    private = dispatch_run.dispatch_preflight.preflight_tasks([{**task, "confidential": True}], tmp_path)
    assert private["status"] == "validated", private
    assert private["routes"][0]["resolved_model"] == "opencode-go/deepseek-v4.1-flash"
    refused = dispatch_run.dispatch_preflight.preflight_tasks([{**task, "alias": None, "model": "opencode/mimo-v2.6-flash-free",
                                             "confidential": True}], tmp_path)
    assert refused["error"] == "confidential_route" and "fix:" in refused["fix"]


def test_routes_health_and_help_routes_print_the_same_view(tmp_path):
    env = overlay_environment(tmp_path, {})
    plain = model_route(env, "routes", "--json", "--health")
    assert plain.returncode == 0, plain.stderr
    document = json.loads(plain.stdout)
    assert {"routes", "health"} <= set(document)
    assert "--json" in model_route(env, "routes", "--help").stdout


def native_catalog():
    """A fixed pool so the native-first tests do not track catalogue preference edits."""
    catalog = json.loads(json.dumps(CATALOG))
    catalog["routes"]["senior"] = [
        {"model": "claude/claude-opus-5-5", "weight": "high"},
        {"model": "codex/gpt-6.1-sol", "weight": "normal"},
        {"model": "agy/gemini-3.8-flash", "weight": "normal"},
    ]
    return catalog


def test_native_seat_skips_its_own_models_in_top_and_rotate(tmp_path):
    catalog = native_catalog()
    top = pick({"route": "senior", "native": "claude"}, tmp_path, catalog=catalog)
    assert models(top) == ["codex/gpt-6.1-sol"]
    assert "claude/claude-opus-5-5 skipped: use a native subagent" in top["warnings"]
    rotated = [models(pick({"route": "senior", "rotate": True, "native": "claude"}, tmp_path, catalog=catalog))[0]
               for _ in range(4)]
    assert not any(model.startswith("claude/") for model in rotated)
    assert models(pick({"route": "senior", "native": "codex"}, tmp_path, catalog=catalog)) == [
        "claude/claude-opus-5-5"]


def test_native_council_hands_its_own_members_back_to_the_chair(tmp_path):
    catalog = native_catalog()
    for seed in range(10):
        result = pick({"route": "senior", "council": 3, "native": "claude"}, tmp_path, catalog=catalog, seed=seed)
        assert sorted(models(result)) == ["agy/gemini-3.8-flash", "codex/gpt-6.1-sol"]
        assert "spawn 1 native claude member (claude-opus-5-5); Fabric runs 2 of 3" in result["warnings"]
        assert [item["reason"] for item in result["picks"]] == ["senior council 1/2", "senior council 2/2"]


def test_native_only_pool_names_the_native_spawn_instead_of_running_it(tmp_path):
    catalog = native_catalog()
    catalog["routes"]["senior"] = [{"model": "claude/claude-opus-5-5", "weight": "high"}]
    for request in ({"route": "senior"}, {"route": "senior", "council": 2}):
        with pytest.raises(pools.PoolError) as error:
            pick({**request, "native": "claude"}, tmp_path, catalog=catalog)
        assert error.value.code == "route_native_only"
        assert "native claude member" in error.value.fix and "claude-opus-5-5" in error.value.fix


def test_an_explicit_native_adapter_or_models_entry_still_runs(tmp_path):
    catalog = native_catalog()
    narrowed = pick({"route": "senior", "adapter": "claude", "native": "claude"}, tmp_path, catalog=catalog)
    assert models(narrowed) == ["claude/claude-opus-5-5"]
    listed = pick({"models": ["claude/claude-opus-5-5", "codex/gpt-6-luna"], "native": "claude"}, tmp_path,
                  catalog=catalog)
    assert models(listed) == ["claude/claude-opus-5-5", "codex/gpt-6-luna"]


def test_models_keeps_an_opencode_provider_path_after_the_adapter_prefix(tmp_path):
    assert models(pick({"models": ["opencode/big-pickle"]}, tmp_path)) == ["opencode/opencode/big-pickle"]
    assert models(pick({"models": ["opencode/opencode-go/brand-new"]}, tmp_path)) == [
        "opencode/opencode-go/brand-new"]
    assert models(pick({"models": ["opencode/anthropic/claude-sonnet-latest"]}, tmp_path)) == [
        "opencode/anthropic/claude-sonnet-latest"]


def test_confidential_pick_skips_an_unregistered_free_model_at_high_weight(tmp_path):
    unlisted = "opencode/opencode/rotated-in-free"
    catalog = {**CATALOG, "routes": {**CATALOG["routes"], "private": [
        {"model": unlisted, "weight": "high"},
        {"model": "codex/gpt-6-luna", "weight": "normal"}]}}
    assert pools.split_model(router, catalog, unlisted)[1] == "opencode/rotated-in-free"
    assert models(pick({"route": "private"}, tmp_path, catalog=catalog)) == [unlisted]
    for seed in range(10):
        private = pick({"route": "private", "confidential": True}, tmp_path, catalog=catalog, seed=seed)
        assert models(private) == ["codex/gpt-6-luna"]


def test_confidential_pick_keeps_the_privacy_of_a_newer_live_family_model(tmp_path, monkeypatch):
    import copy
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    (tmp_path / "capabilities.json").write_text(json.dumps({"codex": {
        "observed_at": datetime.now(timezone.utc).isoformat(), "models": ["gpt-6.2-sol"]}}))
    catalog = copy.deepcopy(CATALOG)
    sol = next(item for item in catalog["adapters"]["codex"]["models"] if item["id"] == "gpt-6.1-sol")
    sol.update(trains_on_prompts=True, plan_cap_usd=0)
    catalog["model_traits"] = {"codex/sol": ["free"]}
    assert {"free", "trains-on-prompts"} <= set(pools.model_traits(router, catalog, "codex", "gpt-6.2-sol"))
    with pytest.raises(pools.PoolError):
        pick({"models": ["codex/sol"], "confidential": True}, tmp_path, catalog=catalog)
    assert models(pick({"models": ["codex/sol"]}, tmp_path, catalog=catalog)) == ["codex/gpt-6.2-sol"]


def test_direct_confidential_dispatch_treats_a_live_family_model_like_its_catalogue_family(tmp_path, monkeypatch):
    import copy
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    (tmp_path / "capabilities.json").write_text(json.dumps({"codex": {
        "observed_at": datetime.now(timezone.utc).isoformat(), "models": ["gpt-6.2-sol"]}}))
    catalog = copy.deepcopy(CATALOG)
    catalog["model_traits"] = {"codex/gpt-6.1-sol": ["trains-on-prompts"]}
    exec_routing = exec_routing_module()
    monkeypatch.setattr(router, "load_catalog", lambda *args, **kwargs: catalog)
    monkeypatch.setattr(exec_routing, "_model_route_module", lambda: router)
    assert exec_routing.disclosure_risk("codex", "gpt-6.1-sol", {})
    assert exec_routing.disclosure_risk("codex", "gpt-6.2-sol", {})


def test_confidential_pick_of_an_effort_suffixed_live_id_keeps_its_family_traits(tmp_path, monkeypatch):
    import copy
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    (tmp_path / "capabilities.json").write_text(json.dumps({"agy": {
        "observed_at": datetime.now(timezone.utc).isoformat(), "models": ["gemini-3.9-flash-high"]}}))
    catalog = copy.deepcopy(CATALOG)
    next(item for item in catalog["adapters"]["agy"]["models"] if item["id"] == "gemini-3.8-flash")["trains_on_prompts"] = True
    assert "trains-on-prompts" in pools.model_traits(router, catalog, "agy", "gemini-3.9-flash-high")
    with pytest.raises(pools.PoolError):
        pick({"models": ["agy/gemini-3.9-flash-high"], "confidential": True}, tmp_path, catalog=catalog)


def test_pool_pick_of_a_suffixed_live_id_keeps_the_effort_its_suffix_names(tmp_path, monkeypatch):
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path))
    (tmp_path / "capabilities.json").write_text(json.dumps({"agy": {
        "observed_at": datetime.now(timezone.utc).isoformat(),
        "models": ["gemini-3.9-flash-low", "gemini-3.9-flash-high"]}}))
    low = pick({"models": ["agy/gemini-3.9-flash-low"]}, tmp_path)["picks"][0]
    assert (low["model"], low.get("effort")) == ("gemini-3.9-flash", "low")
    assert pick({"models": ["agy/gemini-3.9-flash-low@high"]}, tmp_path)["picks"][0]["effort"] == "high"


def _free_catalog(flag):
    entry = {"id": "opencode/zero-retention-free"}
    if flag is not None:
        entry["trains_on_prompts"] = flag
    adapter = {**CATALOG["adapters"]["opencode"], "models": [entry]}
    router._apply_free_pattern(adapter)
    return {**CATALOG, "adapters": {**CATALOG["adapters"], "opencode": adapter}, "routes": {
        **CATALOG["routes"], "zr": [{"model": "opencode/opencode/zero-retention-free", "weight": "high"},
                                    {"model": "codex/gpt-6-luna", "weight": "sparing"}]}}


def test_free_model_with_explicit_no_training_is_not_private_unsafe(tmp_path):
    catalog = _free_catalog(False)
    adapter = catalog["adapters"]["opencode"]
    assert not adapter["models"][0].get("warning")
    traits = pools.model_traits(router, catalog, "opencode", "opencode/zero-retention-free")
    assert "free" in traits and "trains-on-prompts" not in traits
    for seed in range(5):
        result = pick({"route": "zr", "confidential": True}, tmp_path, catalog=catalog, seed=seed)
        assert models(result) == ["opencode/opencode/zero-retention-free"]
        assert not any("may train" in w or "skipped" in w for w in result["warnings"])
    assert not any("may train" in w for w in pick({"route": "zr"}, tmp_path, catalog=catalog)["warnings"])


def test_free_model_without_flag_still_defaults_to_training(tmp_path):
    catalog = _free_catalog(None)
    assert catalog["adapters"]["opencode"]["models"][0]["warning"]
    traits = pools.model_traits(router, catalog, "opencode", "opencode/zero-retention-free")
    assert {"free", "trains-on-prompts"} <= set(traits)
    for seed in range(5):
        private = pick({"route": "zr", "confidential": True}, tmp_path, catalog=catalog, seed=seed)
        assert models(private) == ["codex/gpt-6-luna"]


def test_exec_routing_treats_explicit_no_training_free_model_as_safe():
    exec_routing = exec_routing_module()
    assert exec_routing.disclosure_risk("opencode", "opencode/deepseek-v4.1-flash-free",
                                        {"plan_cap_usd": 0, "trains_on_prompts": False}) is None
    assert exec_routing.disclosure_risk("opencode", "opencode/mimo-v2.6-flash-free", {})


def test_a_configured_training_trait_outranks_a_no_training_flag(monkeypatch):
    import copy
    catalog = copy.deepcopy(_free_catalog(False))
    catalog["model_traits"] = {"opencode/opencode/zero-retention-free": ["trains-on-prompts"]}
    exec_routing = exec_routing_module()
    monkeypatch.setattr(router, "load_catalog", lambda *args, **kwargs: catalog)
    monkeypatch.setattr(exec_routing, "_model_route_module", lambda: router)
    # Direct dispatch and fallback checks pass the resolver's flag; the trait still wins, as in the pools.
    assert exec_routing.disclosure_risk("opencode", "opencode/zero-retention-free",
                                        {"plan_cap_usd": 0, "trains_on_prompts": False})
    assert exec_routing.disclosure_risk("opencode", "opencode/zero-retention-free", {})


def test_an_adapter_wide_no_training_default_never_clears_an_unknown_free_model(tmp_path):
    catalog = _free_catalog(None)
    adapter = {**catalog["adapters"]["opencode"], "trains_on_prompts": False}
    catalog = {**catalog, "adapters": {**catalog["adapters"], "opencode": adapter}}
    assert router.model_route_live.training_flag(adapter, None, "opencode/rotating-free") is None
    assert "trains-on-prompts" in pools.model_traits(router, catalog, "opencode", "opencode/rotating-free")
    assert router.model_route_live.training_flag(adapter, None, "opencode-go/paid-model") is False


def test_a_trait_on_the_canonical_id_follows_an_effort_suffixed_pick(monkeypatch):
    import copy
    catalog = copy.deepcopy(CATALOG)
    catalog["model_traits"] = {"cursor/grok-4.7": ["trains-on-prompts"]}
    assert "trains-on-prompts" in pools.model_traits(router, catalog, "cursor", "grok-4.7-high")
    exec_routing = exec_routing_module()
    monkeypatch.setattr(router, "load_catalog", lambda *args, **kwargs: catalog)
    monkeypatch.setattr(exec_routing, "_model_route_module", lambda: router)
    assert exec_routing.disclosure_risk("cursor", "grok-4.7-high", {"trains_on_prompts": False})


def test_every_free_sign_keeps_an_adapter_wide_false_from_clearing_an_unknown_model():
    adapter = {**SEED["adapters"]["opencode"], "trains_on_prompts": False}
    for model in ("opencode/big-pickle", "openrouter/example:free", "opencode/rotating-free"):
        assert router.model_route_live.training_flag(adapter, None, model) is None, model
        assert "trains-on-prompts" in pools.model_traits(
            router, {**SEED, "adapters": {**SEED["adapters"], "opencode": adapter}}, "opencode", model), model


def _quota_reading(quota, provider, windows, observed, account="default", **extra):
    """A cached provider quota reading, as `provenant quota` leaves it (#1030)."""
    stamp = observed.isoformat().replace("+00:00", "Z")
    quota.save_snapshot({
        "schema": "provenant.quota.v1", "provider": provider, "account": account, "plan": None, "host": "studio",
        "active": True, "priority": None, "observed_at": stamp, "attempted_at": stamp, "source": "test",
        "status": "ok", "error": None, "unreported": [], "credits": None,
        "windows": [{"name": name, "raw_kind": name, "used_percent": used, "window_minutes": minutes,
                     "reset_at": (observed + reset_in).isoformat().replace("+00:00", "Z"), "window_start": None}
                    for name, used, minutes, reset_in in windows], **extra}, quota.provider(provider).default_identity())


def test_routes_preserve_weights_for_burst_with_reserves_and_block_real_shortage(tmp_path, monkeypatch, capsys):
    """Claude at 3% must not stay cooling on a stale usage cooldown; a Codex burst with reserves stays available."""
    env = overlay_environment(tmp_path, {})
    home, state = tmp_path / "home", tmp_path / "state"
    env.update(HOME=str(home), CLAUDE_CONFIG_DIR=str(home / ".claude"), CODEX_HOME=str(home / ".codex"),
               AGENT_FABRIC_STATE_DIRECTORY=str(state), FABRIC_COOLDOWNS_PATH=str(state / "cooldowns.json"),
               AGENT_FABRIC_ROUTE_HEALTH_PATH=str(state / "route-health.json"), PROVENANT_QUOTA="1")
    for name in ("CLAUDE_CONFIG_DIR", "CODEX_HOME"):
        Path(env[name]).mkdir(parents=True)
    hosts = Path(env['AGENT_FABRIC_INSTANCE_ROOT']) / '.agent-fabric/hosts.json'
    hosts.parent.mkdir(parents=True, exist_ok=True)
    hosts.write_text(json.dumps({'schema_version': 1, 'local_host': 'studio'}))
    state.mkdir()
    now = datetime.now(timezone.utc)

    class FixedDatetime(datetime):
        @classmethod
        def now(cls, tz=None):
            return now.astimezone(tz)

    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    for name in ("claude", "codex"):
        (bin_dir / name).write_text("#!/bin/sh\nexit 97\n")
        (bin_dir / name).chmod(0o755)
    env["PATH"] = str(bin_dir)
    for key, value in env.items():
        monkeypatch.setenv(key, value)

    # Exercise the CLI entry point in process so every routing clock is fixed.
    quota_router = load("quota_pools_router_under_test", ROOT / "scripts/model_route.py")
    exec_routing = exec_routing_module()
    monkeypatch.setattr(quota_router.model_route_live, "_quota", None)
    monkeypatch.setattr(exec_routing, "_QUOTA", None)
    for quota in (quota_router.model_route_live._provider_quota(), exec_routing._provider_quota()):
        monkeypatch.setattr(quota, "utcnow", lambda: now)
    quota = quota_router.model_route_live._provider_quota()
    monkeypatch.setattr(quota_router.model_route_live, "datetime", FixedDatetime)
    fabric_records = importlib.import_module(exec_routing.read_cooldowns.__module__)
    monkeypatch.setattr(fabric_records, "datetime", FixedDatetime)

    def routes(*args):
        assert quota_router.main(["routes", *args]) == 0
        return capsys.readouterr().out

    later = (now + timedelta(hours=8)).isoformat()
    (state / "cooldowns.json").write_text(json.dumps({"schema": "fabric.cooldowns.v1", "cooldowns": {
        "claude/*": {"adapter": "claude", "model": "*", "status": "usage_limited", "cooling_until": later,
                     "recorded_at": (now - timedelta(hours=1)).isoformat()}}}))
    assert exec_routing.cooling("claude", "claude-opus-5-5")["cooling_until"] == later
    _quota_reading(quota, "claude", [("five_hour", 3, 300, timedelta(hours=2)),
                                     ("seven_day", 20, 10080, timedelta(days=4))], now - timedelta(minutes=5))
    before = json.loads(routes("--json"))
    _quota_reading(quota, "codex", [("secondary", 33, 10080, timedelta(hours=159))], now,
                   usage_credits=[{"balance": 26000, "unit": "credits"}], credits={"reset_credits": 1})
    document = json.loads(routes("--json"))
    entries = [entry for pool_entries in document["routes"].values() for entry in pool_entries]
    claude = [entry for entry in entries if entry["model"].startswith("claude/")]
    codex = [entry for entry in entries if entry["model"].startswith("codex/")]
    assert claude and all(entry["availability"] == "available" for entry in claude)
    assert claude[0]["quota"]["state"] == "reserve"
    assert codex and all(entry["availability"] == "available" for entry in codex)
    assert codex[0]["quota"]["state"] == "ahead_of_pace"
    assert "allowance lasts about 18h" in codex[0]["quota"]["summary"]
    assert "26,000 credits" in codex[0]["quota"]["summary"] and "1 banked reset" in codex[0]["quota"]["summary"]
    assert {name: [(entry["model"], entry["weight"]) for entry in pool] for name, pool in document["routes"].items()} == {
        name: [(entry["model"], entry["weight"]) for entry in pool] for name, pool in before["routes"].items()}
    text = routes()
    assert any(line.lstrip().startswith("codex/") and "[quota secondary: ahead of pace" in line for line in text.splitlines())

    # An exhausted window cools the provider until its reset, in routes and in dispatch's cooldown check.
    reset = now + timedelta(hours=3)
    _quota_reading(quota, "codex", [("primary", 100, 300, reset - now)], now)
    document = json.loads(routes("--json"))
    codex = [entry for pool_entries in document["routes"].values() for entry in pool_entries
             if entry["model"].startswith("codex/")]
    assert all(entry["availability"].startswith("cooling until ") for entry in codex)
    blocked = exec_routing.cooling("codex", "gpt-6-luna")
    assert blocked["status"] == "quota_exhausted"
    assert blocked["cooling_until"] == reset.isoformat().replace("+00:00", "Z")
    assert exec_routing.cooling("claude", "claude-opus-5-5") is None


@pytest.mark.parametrize('adapter', ['claude', 'codex', 'agy', 'cursor', 'opencode', 'kiro'])
def test_attempt_scope_is_frozen_before_identity_environment_changes(tmp_path, monkeypatch, adapter):
    from types import SimpleNamespace
    mod = load("scope_dispatch_run_under_test", ROOT / "skills/orchestrate/scripts/dispatch_run.py")
    monkeypatch.setenv('CLAUDE_CONFIG_DIR', str(tmp_path / 'a'))
    args = SimpleNamespace(tool=adapter, alias='senior', model='sonnet', alias_supplied=True,
                           effort=None, task_id='scope-test', access_mode='read_only', worktree=None,
                           _phase_timings={})
    running = mod.contract_row(args, tmp_path, 1, tmp_path / 'attempt', {}, 'now')
    assert running['account'] is not None and running['host']
    monkeypatch.setenv('CLAUDE_CONFIG_DIR', str(tmp_path / 'b'))
    terminal = mod.contract_row(args, tmp_path, 1, tmp_path / 'attempt', {}, 'now')
    assert (terminal['account'], terminal['host']) == (running['account'], running['host'])
    if adapter == 'claude':
        following = mod.contract_row(args, tmp_path, 2, tmp_path / 'attempt-2', {}, 'now')
        assert following['account'] != running['account']


def test_utility_defaults_private_and_never_admits_free_or_training_models(tmp_path):
    assert 'utility' in SEED['routes']
    for entry in SEED['routes']['utility']:
        if entry.get('confidential', True):
            adapter, model = pools.split_model(router, SEED, entry['model'])
            traits = pools.model_traits(router, SEED, adapter, model)
            assert not {'free', 'trains-on-prompts'}.intersection(traits), entry['model']
    private = pick({'route': 'utility', 'council': 8}, tmp_path, catalog=SEED)
    assert private['confidential'] is True
    assert set(models(private)) == {'claude/claude-haiku-5-5', 'codex/gpt-6-luna'}
    for member in private['picks']:
        traits = pools.model_traits(router, SEED, member['adapter'], member['model'])
        assert not {'free', 'trains-on-prompts'}.intersection(traits)
        assert member['effort'] == 'low'
    with pytest.raises(pools.PoolError, match='No available model'):
        pick({'route': 'digest'}, tmp_path, catalog=SEED, installed=lambda a: a == 'opencode')
    public = pick({'route': 'classify', 'confidential': False}, tmp_path,
                  catalog=SEED, installed=lambda a: a == 'opencode')
    assert public['picks'][0]['adapter'] == 'opencode'


def test_utility_rejects_malformed_confidentiality_in_requests_and_defaults(tmp_path):
    for malformed in ('true', 'false', None, 0):
        with pytest.raises(pools.PoolError, match='confidential must be a boolean'):
            pick({'route': 'utility', 'confidential': malformed}, tmp_path, catalog=SEED)
        catalog = json.loads(json.dumps(SEED))
        catalog['route_defaults']['utility']['confidential'] = malformed
        with pytest.raises(pools.PoolError, match='confidential must be a boolean'):
            pick({'route': 'utility'}, tmp_path, catalog=catalog)


def test_utility_stays_private_without_catalogue_defaults(tmp_path):
    catalog = json.loads(json.dumps(SEED))
    catalog.pop('route_defaults')
    private = pick({'route': 'utility', 'council': 8}, tmp_path, catalog=catalog)
    assert private['confidential'] is True
    assert set(models(private)) == {'claude/claude-haiku-5-5', 'codex/gpt-6-luna'}


@pytest.mark.parametrize('malformed', ['true', 'false', None, 0, 1])
@pytest.mark.parametrize('location', ['request', 'default', 'entry'])
def test_utility_rejects_non_boolean_pool_policy(tmp_path, malformed, location):
    catalog = json.loads(json.dumps(SEED))
    request = {'route': 'utility'}
    if location == 'request':
        request['allow_free'] = malformed
    elif location == 'default':
        catalog['route_defaults']['utility']['allow_free'] = malformed
    else:
        catalog['routes']['utility'][0]['confidential'] = malformed
    field = 'confidential' if location == 'entry' else 'allow_free'
    with pytest.raises(pools.PoolError, match=field + ' must be a boolean'):
        pick(request, tmp_path, catalog=catalog)
