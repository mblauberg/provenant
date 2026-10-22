"""Shared delivery review route-lineage policy."""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass


@dataclass(frozen=True)
class DeliveryLineageEligibility:
    identity_and_certification_agree: bool
    cross_family_agrees: bool
    provider_family: str

    @property
    def eligible(self) -> bool:
        return self.identity_and_certification_agree and self.cross_family_agrees


def delivery_lineage_eligibility(
    route: Mapping[str, object],
    *,
    adapter: object,
    reviewer_id: object,
    model: object,
    model_family: object,
    chair_family: str | None,
) -> DeliveryLineageEligibility:
    """Evaluate review identity, certification, and cross-family agreement."""
    raw_provider_family = route.get("provider_family")
    has_provider_family = (
        isinstance(raw_provider_family, str) and bool(raw_provider_family.strip())
    )
    provider_family = raw_provider_family.strip() if has_provider_family else ""
    certified = (
        route.get("certification_eligible") is True
        or (
            route.get("certification_eligible") is False
            and chair_family
            and raw_provider_family == chair_family
        )
    )
    identity_agrees = (
        route.get("status") == "ok"
        and route.get("adapter") == adapter
        and route.get("reviewer_id") == reviewer_id
        and route.get("resolved_model", route.get("model")) == model
        and route.get("model_family") == model_family
        and has_provider_family
        and certified
    )
    cross_family_agrees = (
        provider_family == chair_family or route.get("cross_family") is True
    )
    return DeliveryLineageEligibility(
        identity_and_certification_agree=bool(identity_agrees),
        cross_family_agrees=bool(cross_family_agrees),
        provider_family=provider_family,
    )
