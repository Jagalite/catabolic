# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Bounded published-resolution metadata with authorization before pagination."""

from .access import AccessError
from .domain import CatabolicError


def page(store, profile, catalog, *, first=100, after="", access=None):
    if type(first) is not int or not 1 <= first <= 1000:
        raise CatabolicError("resolution page size must be between 1 and 1000")
    if access and not any(
        g.get("operator") or catalog in g.get("projection_ids", [])
        for g in access.matching("metadata:read")
    ):
        raise AccessError("not_found", 404)
    # HTTP callers install Catalog's admitted items/files views before this call.
    restriction = (
        " AND e.item_id IN (SELECT id FROM items) AND (e.file_id IS NULL OR e.file_id IN (SELECT id FROM files))"
        if access
        else ""
    )
    rows = store.rows(
        "SELECT e.id,e.item_id,e.file_id,e.revision,e.policy_id,e.state,e.tier,e.generation,e.published_generation FROM fallback_entries e WHERE e.profile=? AND e.catalog=? AND e.active=1 AND e.id>?"
        + restriction
        + " ORDER BY e.id LIMIT ?",
        (profile, catalog, after or "", first + 1),
    )
    return {
        "nodes": rows[:first],
        "pageInfo": {
            "hasNextPage": len(rows) > first,
            "endCursor": rows[first - 1]["id"] if len(rows) > first else None,
        },
    }
