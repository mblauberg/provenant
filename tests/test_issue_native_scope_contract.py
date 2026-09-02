from pathlib import Path
import json

import yaml


ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text()


def test_shipped_issue_form_matches_the_setup_repo_template_and_lifecycle_contract():
    live_form = read(".github/ISSUE_TEMPLATE/work-item.yml")
    template_form = read("skills/setup-repo/templates/ISSUE_TEMPLATE/work-item.yml")

    assert live_form == template_form


def test_pre_rename_lifecycle_routing_dataset_is_explicitly_historical():
    dataset = yaml.safe_load(read("evals/lifecycle-routing.yaml"))

    assert dataset["status"] == "historical"
    assert dataset["catalogue_state"] == "pre-autopilot-rename"
    assert "not a current routable skill" in dataset["note"]
