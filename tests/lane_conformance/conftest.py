"""FIFO-controlled real dispatch fixtures."""

import pytest

from tests.support.dispatch import owner_command

from .support import Lane


def pytest_configure(config):
    try:
        owner_command()
    except ValueError as error:
        raise pytest.UsageError(str(error)) from error


@pytest.fixture
def lane(tmp_path, monkeypatch):
    lanes = []

    def create(name="lane", startup=30):
        driver = Lane(tmp_path / name, monkeypatch, startup)
        lanes.append(driver)
        return driver

    yield create
    first_error = None
    for driver in reversed(lanes):
        try:
            driver.close()
        except BaseException as error:
            if first_error is None:
                first_error = error
    if first_error is not None:
        raise first_error
