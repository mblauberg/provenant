"""Write the machine-local Agent Fabric product-root pointer."""

from __future__ import annotations

import argparse
from pathlib import Path
import sys

from lib.product_root_resolver import write_pointer_file


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("instance_root", type=Path)
    parser.add_argument("product_root", type=Path)
    arguments = parser.parse_args()
    try:
        write_pointer_file(arguments.instance_root, arguments.product_root)
    except (OSError, ValueError) as exc:
        print(f"conflicting: {exc}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
