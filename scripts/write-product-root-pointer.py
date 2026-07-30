#!/usr/bin/env python3
"""Write the machine-local Agent Fabric product-root pointer."""

from __future__ import annotations

import argparse
from pathlib import Path

from lib.product_root_resolver import write_pointer_file


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("instance_root", type=Path)
    parser.add_argument("product_root", type=Path)
    arguments = parser.parse_args()
    write_pointer_file(arguments.instance_root, arguments.product_root)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
