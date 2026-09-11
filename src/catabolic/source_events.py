# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Shared event adapter: hints invalidate evidence, never publish inventory."""

from .observations import dirty
from .watchers import dirty as dirty_watchers


def invalidate(app, source, *, reason="event"):
    # Full-source observations are the current coverage unit. Path hints are not
    # retained, bounding the queue regardless of native event volume.
    if app.store.rows(
        "SELECT 1 FROM generated_locations WHERE profile=? AND location=?",
        (app.profile, source),
    ):
        return {"ignored": "generated_source_uses_completion_events"}
    dirty(app, source)
    with app.store.transaction() as db:
        dirty_watchers(db, app.profile)
    return {"source": source, "reason": reason, "coverage": "full_source"}


def startup(app):
    return [
        invalidate(app, r["owner"], reason="startup_reconciliation")
        for r in app.store.rows(
            "SELECT owner FROM bindings WHERE profile=? AND kind='source'",
            (app.profile,),
        )
    ]
