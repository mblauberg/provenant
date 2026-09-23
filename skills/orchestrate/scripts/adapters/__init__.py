"""Declarative provider profiles. Execution and classification belong to the supervisor."""

from importlib import import_module

NAMES = ("claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot")


def profile(name):
    if name not in NAMES:
        raise ValueError(f"unknown adapter: {name}")
    return import_module(f"{__name__}.{name}")
