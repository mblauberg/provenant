"""Provider profiles; execution and classification belong to the supervisor.

Optional hooks: continues_turn(event), diagnostics(stderr) -> (text, reason),
environment(plan) -> env overrides, text_event(text) -> recovery event.
Absent hooks preserve the turn/log/env and recover text as a result event.
EXIT_AFTER_OUTPUT defaults to False; EMPTY_FAILURE defaults to an empty reason.
"""

from importlib import import_module

from .dispatch_vocabulary import IMPLEMENTED as NAMES


def profile(name):
    if name not in NAMES:
        raise ValueError(f"unknown adapter: {name}")
    return import_module(f"{__name__}.{name}")
