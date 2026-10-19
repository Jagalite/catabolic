"""Explicit native fallback maintenance; disabled until configured by the caller."""

import json

from . import _native


def configure(app, catalog, *, enabled, interval=60, max_changes=100, max_removals=0):
    request = dict(
        catalog=catalog,
        interval=interval,
        max_changes=max_changes,
        max_removals=max_removals,
    )
    app.store._call("fallback", "worker-configure-validate", request, app.profile)
    request["enabled"] = int(enabled)
    return app.store._call("fallback", "worker-configure", request, app.profile)


def tick(database, profile="default", *, limit=10):
    return json.loads(
        _native.fallback_tick(
            str(database),
            profile,
            json.dumps(
                dict(limit=int(limit) if isinstance(limit, bool) else limit),
                allow_nan=False,
            ),
        )
    )
