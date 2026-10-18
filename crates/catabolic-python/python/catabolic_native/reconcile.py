"""Ordinary reconciliation through the caller's persistent native Store.

Apply and recover use the shared native journal executor. Its existing gates
(symlink replace/remove, hardlink retirement, fallback-bound catalogs) apply.
"""

from __future__ import annotations

import json
import math

from .app import Application


def _publish(reconciler, operation, request, after_filesystem):
    """Run apply/recover in the shared native executor on the caller's Store."""
    request["notify_consumers"] = bool(reconciler.notify_consumers)
    return json.loads(
        reconciler.store._owner.reconcile_publish(
            operation,
            json.dumps(request, allow_nan=False),
            reconciler.profile,
            after_filesystem,
        )
    )


class Reconciler:
    def __init__(self, app: Application, *, notify_consumers=True):
        self.app = app
        self.store = app.store
        self.profile = app.profile
        self.notify_consumers = notify_consumers

    def owner(self, catalog: str) -> dict:
        return self.store._call(
            "reconcile", "owner", {"catalog": catalog}, self.profile
        )

    def catalogs(self, catalog: str | None) -> list[str]:
        return self.store._call(
            "reconcile", "catalogs", {"catalog": catalog}, self.profile
        )

    def preview(
        self,
        catalog: str | None = "global",
        *,
        max_removals=None,
        max_removal_percent=None,
    ) -> dict:
        request = {
            "catalog": catalog,
            "max_removals": max_removals
            if type(max_removals) is int or max_removals is None
            else "invalid",
            "max_removal_percent": max_removal_percent,
        }
        if max_removal_percent is not None and (
            type(max_removal_percent) not in (int, float)
            or not math.isfinite(max_removal_percent)
        ):
            request["max_removal_percent"] = "invalid"
        result = self.store._call("reconcile", "preview", request, self.profile)
        # Restore the frozen Python scalar kinds after native JSON transport.
        # This does not alter the native plan, budgets, or safety decision.
        budget = result["removal_budget"]
        budget["max_removal_percent"] = max_removal_percent
        if budget["owned_entries"] == 0:
            budget["percent"] = 0
        return result

    def verify(self, catalog: str | None = "global") -> dict:
        return self.store._call(
            "reconcile", "verify", {"catalog": catalog}, self.profile
        )

    def apply(
        self,
        catalog: str | None = "global",
        *,
        after_filesystem=None,
        max_changes=None,
        max_removals=None,
        max_removal_percent=None,
    ) -> dict:
        request = {
            "catalog": catalog,
            "max_removals": max_removals
            if type(max_removals) is int or max_removals is None
            else "invalid",
            "max_removal_percent": max_removal_percent,
            "max_changes": max_changes
            if type(max_changes) is int or max_changes is None
            else "invalid",
        }
        if max_removal_percent is not None and (
            type(max_removal_percent) not in (int, float)
            or not math.isfinite(max_removal_percent)
        ):
            request["max_removal_percent"] = "invalid"
        return _publish(self, "apply", request, after_filesystem)

    def recover(
        self,
        catalog: str | None = "global",
        *,
        after_filesystem=None,
        cancel_unapplied=False,
    ) -> dict:
        request = {"catalog": catalog, "cancel_unapplied": bool(cancel_unapplied)}
        return _publish(self, "recover", request, after_filesystem)
