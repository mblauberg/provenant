"""Helpers for tests of provider confinement."""

import importlib


def force_sandbox(monkeypatch, path):
    module = importlib.import_module("provider_confinement")
    monkeypatch.setattr(module, "_sandbox_exec_path", lambda: path)
