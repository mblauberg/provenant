from __future__ import annotations

from copy import deepcopy

import pytest

from skills._shared import review_panel
from skills._shared.review_ladder import SKIPPED_STATUSES


def member(review_id: str, status: str = "complete") -> dict[str, str]:
    return {"id": review_id, "status": status}


def council_spec(**overrides: object) -> dict[str, object]:
    return review_panel.resolve_panel(
        "council",
        membership=["design", "ux", "product", "challenge"],
        **overrides,
    )


def test_panel_axes_and_presets_are_small_and_declarative():
    assert review_panel.PANEL_AXES == (
        "membership",
        "distribution",
        "reduction",
        "degradation",
    )
    assert review_panel.PANEL_PRESETS == {
        "council": {
            "distribution": "shared",
            "reduction": "agreements-conflicts",
            "minimum_members": 3,
        },
        "breadth": {
            "distribution": "split",
            "reduction": "union",
            "minimum_members": 1,
        },
    }
    assert set(review_panel.REDUCERS) == {"agreements-conflicts", "union"}
    assert all(callable(reducer) for reducer in review_panel.REDUCERS.values())


def test_resolution_is_preset_then_per_axis_override_without_mutating_preset():
    presets = deepcopy(review_panel.PANEL_PRESETS)

    spec = review_panel.resolve_panel(
        "council",
        membership=["a", "b"],
        distribution="split",
        reduction="union",
        degradation={"minimum_members": 1},
    )

    assert spec == {
        "preset": "council",
        "membership": ["a", "b"],
        "distribution": "split",
        "reduction": "union",
        "degradation": {"minimum_members": 1},
    }
    assert review_panel.PANEL_PRESETS == presets


def test_resolution_rejects_two_override_channels():
    with pytest.raises(ValueError, match="one panel override form"):
        review_panel.resolve_panel(
            "council",
            {"distribution": "split"},
            distribution="shared",
        )


@pytest.mark.parametrize(
    ("spec", "members"),
    (
        (None, None),
        ({"preset": "council"}, []),
        (
            {
                "preset": "council",
                "membership": "review-a",
                "distribution": "shared",
                "reduction": "agreements-conflicts",
                "degradation": {"minimum_members": 3},
            },
            [],
        ),
        (council_spec(), "not-members"),
        (council_spec(reduction="missing"), []),
        (council_spec(distribution=[]), []),
        (
            council_spec(),
            [
                member("design"),
                member("ux"),
                {"id": "product", "status": []},
                member("challenge"),
            ],
        ),
    ),
)
def test_validate_panel_returns_errors_instead_of_raising_for_user_data(spec, members):
    errors = review_panel.validate_panel(spec, members)

    assert isinstance(errors, list)
    assert errors
    assert all(isinstance(error, str) for error in errors)


def test_unknown_reducer_is_a_validation_error_without_fallback():
    spec = council_spec(reduction="weighted-vote")

    errors = review_panel.validate_panel(
        spec,
        [member(review_id) for review_id in spec["membership"]],
    )

    assert any("reduction is unknown" in error for error in errors)


def test_distribution_is_closed_over_the_preset_table():
    """A misspelled distribution is rejected, not silently recorded.

    Nothing branches on `distribution`, so validation is the only thing that can
    catch a typo. The valid set is derived from the presets, so this stays
    closed without costing a second edit when a preset is added.
    """

    assert review_panel.known_distributions() == frozenset({"shared", "split"})

    spec = council_spec(distribution="shraed")
    errors = review_panel.validate_panel(
        spec,
        [member(review_id) for review_id in spec["membership"]],
    )

    assert any("distribution is unknown" in error for error in errors)


@pytest.mark.parametrize("skipped_status", sorted(SKIPPED_STATUSES))
def test_skipped_members_do_not_count_and_shortfall_never_invalidates_panel(skipped_status):
    spec = council_spec()
    members = [
        member("design"),
        member("ux"),
        member("product", skipped_status),
        member("challenge", skipped_status),
    ]

    assert review_panel.validate_panel(spec, members) == []
    result = review_panel.panel_result(
        spec,
        members,
        {
            "agreements": ["Keep the decision reversible."],
            "conflicts": ["Navigation placement remains contested."],
            "minority_views": ["Retain the current navigation."],
        },
    )
    assert result["shortfall"] == {
        "minimum_members": 3,
        "completed_members": 2,
        "missing_members": 1,
    }
    assert review_panel.validate_panel_result(spec, members, result) == []


def test_agreements_conflicts_preserves_dissent_without_scores_or_votes():
    output = review_panel.REDUCERS["agreements-conflicts"]({
        "agreements": ["A"],
        "conflicts": ["B versus C"],
        "minority_views": ["C"],
    })

    assert output == {
        "agreements": ["A"],
        "conflicts": ["B versus C"],
        "minority_views": ["C"],
    }
    assert not {"score", "weight", "average", "votes"} & set(output)


def test_union_reducer_records_one_deduplicated_breadth_result():
    output = review_panel.REDUCERS["union"]({
        "findings": ["A", "B", "A"],
    })

    assert output == {"findings": ["A", "B"]}


def test_result_schema_and_shortfall_are_machine_checked():
    spec = council_spec()
    members = [member(review_id) for review_id in spec["membership"]]
    result = review_panel.panel_result(
        spec,
        members,
        {"agreements": [], "conflicts": [], "minority_views": []},
    )

    assert result["shortfall"] is None
    assert review_panel.validate_panel_result(spec, members, result) == []
    result["shortfall"] = {
        "minimum_members": 3,
        "completed_members": 2,
        "missing_members": 1,
    }
    assert any(
        "shortfall does not match" in error
        for error in review_panel.validate_panel_result(spec, members, result)
    )


def test_third_preset_needs_only_one_row_and_one_registered_reducer(monkeypatch):
    def risks(output: object) -> dict[str, list[str]]:
        if not isinstance(output, dict) or set(output) != {"risks"}:
            raise ValueError("risks output must contain risks")
        values = output["risks"]
        if not isinstance(values, list) or any(
            not isinstance(value, str) or not value for value in values
        ):
            raise ValueError("risks must be non-empty strings")
        return {"risks": list(dict.fromkeys(values))}

    monkeypatch.setitem(
        review_panel.PANEL_PRESETS,
        "risk-scan",
        {
            "distribution": "sequenced",
            "reduction": "risks",
            "minimum_members": 2,
        },
    )
    monkeypatch.setitem(review_panel.REDUCERS, "risks", risks)

    spec = review_panel.resolve_panel("risk-scan", membership=["a", "b"])
    members = [member("a"), member("b")]
    result = review_panel.panel_result(spec, members, {"risks": ["A", "A"]})

    assert review_panel.validate_panel_result(spec, members, result) == []
    assert spec["distribution"] == "sequenced"
    assert result["output"] == {"risks": ["A"]}
