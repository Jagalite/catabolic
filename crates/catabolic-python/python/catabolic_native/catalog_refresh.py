"""Durable catalog refreshes using the caller's persistent native Store."""

from sqlite3 import Error as SqliteError

from .store import CatabolicError, NativeCommandHandle


def enqueue(db, profile, *, observation_job=None):
    """Queue in the producer's native transaction, preserving its scope token."""
    if not isinstance(db, NativeCommandHandle):
        raise CatabolicError("expected a command handle owned by a native Store")
    db._store._owner.catalog_refresh_enqueue(profile, observation_job, db._token)


def finish(app, result, *, enabled=True):
    """Drain once per batch without copying or serializing the producer result."""
    if enabled and app.store.schema_version >= 14:
        try:
            report = CatalogRefresh(app).run()
        except (CatabolicError, OSError, SqliteError, ValueError) as exc:
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
        request = {
            "catalog": catalog,
            "max_removals": max_removals if type(max_removals) is int else "invalid",
        }
        self.store._call(
            "catalog_refresh", "configure_prevalidate", request, self.profile
        )
        request["enabled"] = bool(enabled)
        self.store._call("catalog_refresh", "configure_validate", request, self.profile)
        request["_enabled_storage"] = int(enabled)
        result = self.store._call("catalog_refresh", "configure", request, self.profile)
        result["enabled"] = enabled
        return result

    def pending(self, *, limit=100):
        return self.store._call(
            "catalog_refresh",
            "pending",
            {"limit": limit if type(limit) is int else "invalid"},
            self.profile,
        )

    def run(self, *, catalog=None, limit=100, force=False):
        request = {
            "catalog": catalog,
            "limit": limit if type(limit) is int else "invalid",
        }
        self.store._call("catalog_refresh", "run_validate", request, self.profile)
        request["force"] = int(force)
        return self.store._call("catalog_refresh", "run", request, self.profile)
