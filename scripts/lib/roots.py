"""Product-root resolution for callers under `scripts/` (issue #754).

The implementation lives in `skills/_shared/roots.py` and this module re-exports
it, so exactly one file decides the root. It cannot live here: the installed
skills catalogue has to be self-sufficient. `install-skills` materialises the
manifest's entries into a tree that carries `skills/` and nothing else, and
`tests/test_install_skills.py::test_materialised_per_entry_layout_makes_the_shared_library_load_bearing`
runs the deliver and orchestrate consumers from exactly that tree under
`python -I`. A skill script reaching into `scripts/lib/` breaks there, whereas
`_shared` travels with the catalogue by design.

The load is by file rather than by import because a caller under `scripts/` may
be invoked directly by path with nothing on `sys.path`.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path

_IMPLEMENTATION = Path(__file__).resolve().parents[2] / "skills" / "_shared" / "roots.py"

_spec = importlib.util.spec_from_file_location("provenant_shared_roots", _IMPLEMENTATION)
if _spec is None or _spec.loader is None:  # pragma: no cover - defensive
    raise ModuleNotFoundError(f"the product-root resolver is missing: {_IMPLEMENTATION}")
_implementation = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_implementation)

ENVIRONMENT_VARIABLE = _implementation.ENVIRONMENT_VARIABLE
product_root = _implementation.product_root
skills_root = _implementation.skills_root

__all__ = ["ENVIRONMENT_VARIABLE", "product_root", "skills_root"]
