"""FIFO-controlled real dispatch fixtures."""

import pytest

from .support import Lane


@pytest.fixture
def lane(tmp_path, monkeypatch):
    lanes = []

    def create(name="lane", startup=30):
        driver = Lane(tmp_path / name, monkeypatch, startup)
        lanes.append(driver)
        return driver

    yield create
    for driver in reversed(lanes):
        driver.close()
