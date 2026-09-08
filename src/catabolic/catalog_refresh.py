# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Durable, opt-in catalog link refreshes; no consumer notifications or scans."""

import sqlite3

from .curation import page_limit
from .domain import CatabolicError
from .layouts import Layouts
from .reconcile import Reconciler


def enqueue(db, profile):
    """Called in the rendition/evidence transaction so a crash cannot lose intent."""
    db.execute(
        """INSERT INTO catalog_refresh_queue(profile,catalog)
        SELECT profile,catalog FROM catalog_refresh_settings WHERE profile=? AND enabled=1
        ON CONFLICT(profile,catalog) DO UPDATE SET
        generation=generation+1,next_attempt=0,error=NULL,updated_at=CURRENT_TIMESTAMP""",
        (profile,),
    )


def finish(app, result, *, enabled=True):
    """Drain once per producer batch; publication failures do not fail the render."""
    if enabled and app.store.schema_version >= 14:
        try:
            report = CatalogRefresh(app).run()
        except (CatabolicError, OSError, sqlite3.Error, ValueError) as exc:
            result["catalog_refresh"] = {
                "complete": False,
                "errors": [{"error": str(exc)[:4000]}],
            }
            return result
        if report["processed"] or report["pending"]:
            result["catalog_refresh"] = report
    return result


class CatalogRefresh:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def configure(self, catalog, *, enabled, max_removals=0):
        self.app.require_recovered()
        if type(max_removals) is not int or max_removals < 0:
            raise CatabolicError("max removals must be a nonnegative integer")
        if not self.store.rows("SELECT id FROM catalogs WHERE id=?", (catalog,)):
            raise CatabolicError("unknown catalog")
        if enabled and not Layouts(self.app)._read("layout-catalog:" + catalog):
            raise CatabolicError(
                "apply a saved layout to the catalog before enabling automatic refresh"
            )
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO catalog_refresh_settings VALUES (?,?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET enabled=excluded.enabled,max_removals=excluded.max_removals",
                (self.profile, catalog, int(enabled), max_removals),
            )
            if enabled:
                db.execute(
                    "INSERT INTO catalog_refresh_queue(profile,catalog) VALUES (?,?) ON CONFLICT(profile,catalog) DO UPDATE SET generation=generation+1,next_attempt=0,error=NULL",
                    (self.profile, catalog),
                )
            else:
                db.execute(
                    "DELETE FROM catalog_refresh_queue WHERE profile=? AND catalog=?",
                    (self.profile, catalog),
                )
        result = {"catalog": catalog, "enabled": enabled, "max_removals": max_removals}
        if enabled:
            result["catalog_refresh"] = self.run(catalog=catalog)
        return result

    def pending(self, *, limit=100):
        page_limit(limit)
        rows = self.store.rows(
            "SELECT q.* FROM catalog_refresh_queue q JOIN catalog_refresh_settings s USING(profile,catalog) WHERE q.profile=? AND s.enabled=1 ORDER BY q.next_attempt,q.catalog LIMIT ?",
            (self.profile, limit + 1),
        )
        return {"pending": rows[:limit], "truncated": len(rows) > limit}

    def run(self, *, catalog=None, limit=100, force=False):
        page_limit(limit)
        if self.store.lock_fd is None or self.store.db.in_transaction:
            raise CatabolicError(
                "catalog refresh requires a writer outside an active transaction"
            )
        rows = self.store.rows(
            """SELECT q.*,s.max_removals FROM catalog_refresh_queue q
            JOIN catalog_refresh_settings s USING(profile,catalog)
            WHERE q.profile=? AND s.enabled=1 AND (? IS NULL OR q.catalog=?)
            AND (? OR q.next_attempt<=unixepoch('now'))
            ORDER BY q.next_attempt,q.updated_at,q.catalog LIMIT ?""",
            (self.profile, catalog, catalog, int(force), limit),
        )
        refreshed, errors = [], []
        for row in rows:
            selected = row["catalog"]
            try:
                reconciler = Reconciler(self.app, notify_consumers=False)
                # Recover any previously journaled writes to this opted-in catalog
                # before replanning. Existing recovery still validates ownership.
                if self.store.rows(
                    "SELECT id FROM journal WHERE profile=? AND catalog=? LIMIT 1",
                    (self.profile, selected),
                ):
                    reconciler.recover(selected)
                self.app.require_recovered()
                layouts = Layouts(self.app)
                owner = layouts._read("layout-catalog:" + selected)
                if not owner:
                    raise CatabolicError("catalog no longer has a saved layout")
                # Stage mappings and validate the entire filesystem plan together;
                # unavailable roots, collisions and budgets roll mappings back.
                with self.store.transaction():
                    layout = layouts.run(
                        owner["layout"], selected, apply=True, limit=limit
                    )
                    if not layout["safe"]:
                        raise CatabolicError(
                            "saved layout has blockers; inspect layout preview"
                        )
                    plan = reconciler.preview(
                        selected, max_removals=row["max_removals"]
                    )
                    if not plan["safe"] or any(
                        a["kind"] == "blocked_source" for a in plan["actions"]
                    ):
                        raise CatabolicError(
                            "link plan has unavailable sources, collisions or removal-budget blockers"
                        )
                synced = reconciler.apply(selected, max_removals=row["max_removals"])
                if not synced["safe"] or not synced["healthy"]:
                    raise CatabolicError(
                        "link synchronization or verification is incomplete"
                    )
                # Generation check preserves newer work if another event is
                # enqueued during this attempt. Normal writers serialize on Store.
                with self.store.transaction() as db:
                    db.execute(
                        "DELETE FROM catalog_refresh_queue WHERE profile=? AND catalog=? AND generation=?",
                        (self.profile, selected, row["generation"]),
                    )
                refreshed.append(selected)
            except (CatabolicError, OSError, sqlite3.Error, ValueError) as exc:
                message = str(exc)[:4000]
                with self.store.transaction() as db:
                    db.execute(
                        "UPDATE catalog_refresh_queue SET attempts=attempts+1,error=?,next_attempt=unixepoch('now')+? WHERE profile=? AND catalog=? AND generation=?",
                        (
                            message,
                            min(3600, 5 * 2 ** min(row["attempts"], 10)),
                            self.profile,
                            selected,
                            row["generation"],
                        ),
                    )
                errors.append({"catalog": selected, "error": message})
        pending = self.store.rows(
            "SELECT count(*) AS n FROM catalog_refresh_queue q JOIN catalog_refresh_settings s USING(profile,catalog) WHERE q.profile=? AND s.enabled=1 AND (? IS NULL OR q.catalog=?)",
            (self.profile, catalog, catalog),
        )[0]["n"]
        return {
            "processed": len(rows),
            "refreshed": refreshed,
            "errors": errors,
            "pending": pending,
            "complete": pending == 0,
        }
