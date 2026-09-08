# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Compatibility delivery for preserved Jellyfin refresh events, outside locks."""

import json
import time
from uuid import uuid4

from . import network_adapters
from .app import Application
from .domain import CatabolicError
from .reconcile import Reconciler
from .store import Store


def drain(path, profile, limit):
    results = []
    for _ in range(limit):
        with Store(path, writable=True) as store:
            rows = store.rows(
                "SELECT * FROM refresh_events WHERE profile=? AND state!='complete' AND attempts<3 AND (lease_until IS NULL OR lease_until<=?) ORDER BY id LIMIT 1",
                (profile, time.time()),
            )
            if not rows:
                break
            row = rows[0]
            target = json.loads(row["target"])
            healthy = Reconciler(Application(store, profile)).verify(row["catalog"])[
                "healthy"
            ]
            if not healthy:
                results.append(
                    {
                        "id": row["id"],
                        "state": "blocked",
                        "error": "publication_unhealthy",
                    }
                )
                break
            lease = str(uuid4())
            with store.transaction() as db:
                db.execute(
                    "UPDATE refresh_events SET attempts=attempts+1,lease_token=?,lease_until=? WHERE id=?",
                    (lease, time.time() + 60, row["id"]),
                )
        try:
            network_adapters.request(
                target["endpoint"] + "/Library/Refresh",
                method="POST",
                headers={
                    "X-Emby-Token": network_adapters.token(target["credential_env"])
                },
            )
            state, error = "complete", None
        except CatabolicError:
            state, error = (
                "failed",
                "legacy refresh delivery failed; inspect endpoint and credential reference",
            )
        with Store(path, writable=True) as store, store.transaction() as db:
            db.execute(
                "UPDATE refresh_events SET state=?,error=?,finished_at=CURRENT_TIMESTAMP,lease_token=NULL,lease_until=NULL WHERE id=? AND lease_token=?",
                (state, error, row["id"], lease),
            )
        results.append({"id": row["id"], "state": state, "error": error})
        if error:
            break
    with Store(path) as store:
        unresolved = store.rows(
            "SELECT count(*) AS n FROM refresh_events WHERE profile=? AND state!='complete'",
            (profile,),
        )[0]["n"]
    return {
        "events": results,
        "unresolved": unresolved,
        "complete": unresolved == 0,
        "indexing_verified": False,
    }
