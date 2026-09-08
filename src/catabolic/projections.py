# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Query + selection policy + reusable layout, using ordinary reconciliation."""

from .copy_selection import CopySelection
from .curation import page_limit
from .domain import CatabolicError
from .layouts import Layouts
from .reconcile import Reconciler
from .rendition_publication import Publication
from .saved_queries import Queries


class _RollbackPreview(Exception):
    pass


class Projections:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def get(self, catalog):
        Publication(self.app).get(catalog)
        rows = self.store.rows(
            "SELECT * FROM projection_bindings WHERE profile=? AND catalog=?",
            (self.profile, catalog),
        )
        owner = Layouts(self.app)._read("layout-catalog:" + catalog)
        binding = (
            rows[0]
            if rows
            else {
                "profile": self.profile,
                "catalog": catalog,
                "query_id": None,
                "layout": owner["layout"] if owner else None,
            }
        )
        return {
            **binding,
            "legacy": not bool(rows),
            "copy_policy": CopySelection(self.app).get(catalog),
            "rendition_policy": Publication(self.app).get(catalog)["definition"],
            "applied_layout": owner["layout"] if owner else None,
            "refresh": self.store.rows(
                "SELECT * FROM catalog_refresh_settings WHERE profile=? AND catalog=?",
                (self.profile, catalog),
            ),
        }

    def put(self, catalog, query_id, layout, *, copies=None, renditions=None):
        self.app.require_recovered()
        self.get(catalog)
        query = Queries(self.store, self.profile).get(query_id)
        if query["definition"]["mode"] != "selection":
            raise CatabolicError("projection requires a complete ID selection")
        Layouts(self.app).get(layout)
        with self.store.transaction() as db:
            if copies is not None:
                CopySelection(self.app).put(catalog, copies, _db=db)
            if renditions is not None:
                Publication(self.app).put(catalog, renditions, _db=db)
            db.execute(
                "INSERT INTO projection_bindings VALUES (?,?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET query_id=excluded.query_id,layout=excluded.layout",
                (self.profile, catalog, query_id, layout),
            )
        return self.get(catalog)

    def run(
        self,
        catalog,
        *,
        apply=False,
        max_removals=0,
        allow_empty=False,
        replace_layout=False,
        limit=100,
    ):
        page_limit(limit)
        if self.store.lock_fd is None or self.store.db.in_transaction:
            raise CatabolicError(
                "projection planning requires the writer lock outside a transaction"
            )
        config = self.get(catalog)
        if not config["layout"]:
            raise CatabolicError("projection has no saved layout")
        result = {}
        reconciler = Reconciler(self.app, notify_consumers=False)
        try:
            with self.store.transaction():
                layout = Layouts(self.app).run(
                    config["layout"],
                    catalog,
                    apply=True,
                    allow_empty=allow_empty,
                    replace_layout=replace_layout,
                    limit=limit,
                )
                result = {
                    "projection": catalog,
                    "layout": layout,
                    "complete": False,
                    "safe": False,
                    "applied": False,
                }
                if not layout["safe"]:
                    raise _RollbackPreview()
                plan = reconciler.preview(catalog, max_removals=max_removals)
                blocked_sources = [
                    a for a in plan["actions"] if a["kind"] == "blocked_source"
                ]
                result["reconciliation"] = {
                    **plan,
                    "actions": plan["actions"][:limit],
                    "action_count": len(plan["actions"]),
                    "actions_truncated": len(plan["actions"]) > limit,
                    "blocked_sources_count": len(blocked_sources),
                    "blocked_sources": blocked_sources[:limit],
                }
                result["safe"] = plan["safe"] and not any(
                    a["kind"] == "blocked_source" for a in plan["actions"]
                )
                if not apply or not result["safe"]:
                    raise _RollbackPreview()
        except _RollbackPreview:
            result["layout"]["applied"] = False
            result["complete"] = result["safe"]
            return result
        result["execution"] = reconciler.apply(catalog, max_removals=max_removals)
        execution = result["execution"]
        execution["applied_count"] = len(execution["applied"])
        execution["details_truncated"] = len(execution["applied"]) > limit
        execution["applied"] = execution["applied"][:limit]
        result["verification"] = reconciler.verify(catalog)
        result["applied"] = True
        result["complete"] = (
            result["execution"]["healthy"] and result["verification"]["healthy"]
        )
        return result
