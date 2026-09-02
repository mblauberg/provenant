"""Approval predicates over a delivery-run receipt's gate fields.

`run_closed` is the closed-run invariant's whole reading of the receipt, and
both the delivery receipt producer and the `implement` checkpoint writer have
to agree on it exactly. It therefore sits here rather than inside either skill,
with `delivery_run_shape` re-exporting it so the validator cone is unaffected.

Constants and pure functions only, with no package-relative import, so this
module can be loaded by file where the skills root is not an import root
without splitting a type identity (#755).
"""

from __future__ import annotations

from typing import Any


def gate_approved(run: dict[str, Any], *path: str) -> bool:
    value: Any = run
    for key in path:
        if not isinstance(value, dict):
            return False
        value = value.get(key)
    return isinstance(value, dict) and value.get("status") == "approved"


def release_approved(run: dict[str, Any]) -> bool:
    return gate_approved(run, "human_gates", "release")


def run_closed(run: dict[str, Any]) -> bool:
    """A run is closed when release is approved and observation has settled."""
    observation = run.get("observation")
    settled = (
        isinstance(observation, dict)
        and observation.get("status") in {"pass", "not_applicable"}
    )
    return release_approved(run) and settled
