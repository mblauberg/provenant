from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[1]
VALIDATOR = ROOT / "skills" / "deliver" / "scripts" / "validate_delivery.py"
VALIDATOR_MODULES = (
    VALIDATOR,
    VALIDATOR.with_name("delivery_validation_common.py"),
    VALIDATOR.with_name("delivery_validation_artifacts.py"),
    VALIDATOR.with_name("delivery_validation_lifecycle.py"),
    VALIDATOR.with_name("delivery_validation_evidence.py"),
    VALIDATOR.with_name("delivery_validation_reviews.py"),
    VALIDATOR.with_name("delivery_validation_security.py"),
    VALIDATOR.with_name("delivery_validation_measures.py"),
)


def test_delivery_validator_coordinator_stays_within_review_cap() -> None:
    assert len(VALIDATOR.read_text().splitlines()) <= 1_000


@pytest.mark.parametrize("validator", VALIDATOR_MODULES)
def test_each_delivery_validator_module_stays_within_review_cap(validator: Path) -> None:
    assert len(validator.read_text().splitlines()) <= 1_000
