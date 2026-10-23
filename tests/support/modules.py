"""Load a script file as a module."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from types import ModuleType


def load_script(path: str | Path, name: str | None = None, *, register: bool = False) -> ModuleType:
    """Execute ``path`` as a fresh module on every call.

    ``register`` puts it in ``sys.modules`` first: dataclasses resolve
    annotations through ``sys.modules``, so a module loaded straight off a
    path needs it.
    """
    path = Path(path)
    spec = importlib.util.spec_from_file_location(name or path.stem, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    if register:
        sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module
