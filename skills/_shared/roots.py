"""The one resolver for the Agent Fabric product root (issue #754).

Before this module, five patterns answered the same question and disagreed.
`skills/deliver/scripts/delivery_receipt.py` derived the root purely from its
own location and ignored `AGENT_FABRIC_PRODUCT_ROOT`, so it read config from a
different tree than every sibling deliver script whenever a launcher such as
`skills/orchestrate/scripts/cf_dispatch.sh` pinned the root explicitly.

Precedence, in one place:

1. `AGENT_FABRIC_PRODUCT_ROOT` when set to a non-empty value. An explicit
   caller knows better than any derivation. The value is expanded but not
   resolved, so a caller who deliberately points at a symlinked tree keeps the
   path it named.
2. Otherwise this file's own location. `roots.py` always sits at
   `<product root>/skills/_shared/roots.py`. It lives in the shared skill
   library rather than under `scripts/lib/` because the installed skills
   catalogue has to be self-sufficient: `install-skills` materialises a tree
   carrying `skills/` and nothing else, and the deliver and orchestrate
   consumers are run from exactly that tree under `python -I`. `scripts/lib/
   roots.py` re-exports this module for callers on the `scripts/` side.

An empty variable is treated as unset: launchers export it unconditionally and
an empty export means "no opinion", not "the filesystem root".
"""

from __future__ import annotations

import os
from pathlib import Path

ENVIRONMENT_VARIABLE = "AGENT_FABRIC_PRODUCT_ROOT"


def product_root() -> Path:
    """Return the product root every caller must agree on."""
    configured = os.environ.get(ENVIRONMENT_VARIABLE)
    if configured:
        return Path(configured).expanduser()
    return Path(__file__).resolve().parents[2]


def skills_root() -> Path:
    """Return the skills directory of the resolved product root."""
    return product_root() / "skills"
