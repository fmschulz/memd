"""Schema migrations for customer reporting."""

from __future__ import annotations


def add_required_tier(table) -> None:
    """Add the required customer tier column."""

    table.require("tier")
