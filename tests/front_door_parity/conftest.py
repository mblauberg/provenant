"""Fixtures for the front door parity suite (see `parity_support.py`)."""

from __future__ import annotations

import pytest

from parity_support import ROLES, FrontDoor


@pytest.fixture(params=ROLES)
def fd(request, tmp_path) -> FrontDoor:
    """The implementation under test, once per role."""
    return FrontDoor(tmp_path, request.param)


@pytest.fixture
def stub(tmp_path) -> FrontDoor:
    return FrontDoor(tmp_path, "stub")


@pytest.fixture
def dispatcher(tmp_path) -> FrontDoor:
    return FrontDoor(tmp_path, "dispatcher")
