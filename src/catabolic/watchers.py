# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Durable named local-owner demand; existing services evaluate and publish."""

import hashlib
import json
import os
import sqlite3
import time
from uuid import uuid4

from . import observations, plans
from .database_io import is_catalog_busy
from .domain import CatabolicError, name
from .fallback_projection import FallbackProjection
from .publication_lock import serialized
from .store import encode
from .watcher_models import WatcherDefinition
from .watcher_schedule import next_due


class Watchers:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def get(self, watcher):
        rows = self.store.rows(
            "SELECT w.*,d.definition,d.revision,d.digest FROM watchers w JOIN watcher_definitions d ON d.id=w.definition_id WHERE w.profile=? AND w.name=?",
            (self.profile, watcher),
        )
        if not rows:
            raise CatabolicError("unknown watcher")
        return {**rows[0], "definition": json.loads(rows[0]["definition"])}

    def list(self):
        return self.store.rows(
            "SELECT name,definition_id,enabled,next_due,pending_generation,completed_generation,active_run FROM watchers WHERE profile=? ORDER BY name LIMIT 1000",
            (self.profile,),
        )

    def put(self, watcher, definition):
        name(watcher)
        value = WatcherDefinition.model_validate(definition).model_dump()
        plans.describe(self.app, value["plan"])
        if value["reaction"] == "processing":
            from .operations import Operations

            operation = Operations(self.app).get(value["operation_id"])
            if operation["operation_kind"] not in ("analysis", "render"):
                raise CatabolicError("unsupported watcher operation")
        serialized = encode(value)
        digest = hashlib.sha256(serialized.encode()).hexdigest()
        with self.store.transaction() as db:
            old = db.execute(
                "SELECT enabled,active_run FROM watchers WHERE profile=? AND name=?",
                (self.profile, watcher),
            ).fetchone()
            if (
                not old
                and db.execute(
                    "SELECT count(*) FROM watchers WHERE profile=?", (self.profile,)
                ).fetchone()[0]
                >= 1000
            ):
                raise CatabolicError("watcher_definition_limit")
            if old and (old["enabled"] or old["active_run"]):
                raise CatabolicError(
                    "disable and finish watcher before replacing definition"
                )
            prior = db.execute(
                "SELECT id FROM watcher_definitions WHERE profile=? AND name=? AND digest=?",
                (self.profile, watcher, digest),
            ).fetchone()
            identifier = prior[0] if prior else str(uuid4())
            if not prior:
                revision = db.execute(
                    "SELECT coalesce(max(revision),0)+1 FROM watcher_definitions WHERE profile=? AND name=?",
                    (self.profile, watcher),
                ).fetchone()[0]
                db.execute(
                    "INSERT INTO watcher_definitions VALUES (?,?,?,?,?,?,?)",
                    (
                        identifier,
                        self.profile,
                        watcher,
                        revision,
                        serialized,
                        digest,
                        time.time(),
                    ),
                )
            db.execute(
                "INSERT INTO watchers(profile,name,definition_id) VALUES (?,?,?) ON CONFLICT(profile,name) DO UPDATE SET definition_id=excluded.definition_id,baseline=NULL,pending_reaction=NULL,next_due=0,retry_at=0,requested_generation=completed_generation",
                (self.profile, watcher, identifier),
            )
        return self.get(watcher)

    def admit(self, watcher):
        """Persist explicit demand independently of schedule and retry timing."""
        self.get(watcher)
        with self.store.transaction() as db:
            db.execute(
                "UPDATE watchers SET pending_generation=pending_generation+1,requested_generation=pending_generation+1,retry_at=0 WHERE profile=? AND name=?",
                (self.profile, watcher),
            )
        return self.get(watcher)

    def enable(self, watcher, enabled=True, *, transfer=False):
        row = self.get(watcher)
        value = row["definition"]
        plans.describe(self.app, value["plan"])
        with self.store.transaction() as db:
            if enabled and value["reaction"] == "projection":
                catalog = value["plan"]["catalog"]
                owner = db.execute(
                    "SELECT watcher FROM watcher_owners WHERE profile=? AND catalog=?",
                    (self.profile, catalog),
                ).fetchone()
                if owner and owner[0] != watcher:
                    raise CatabolicError("projection already owned by another watcher")
                legacy = db.execute(
                    "SELECT 1 FROM fallback_bindings WHERE profile=? AND catalog=? AND enabled=1 UNION SELECT 1 FROM catalog_refresh_settings WHERE profile=? AND catalog=? AND enabled=1",
                    (self.profile, catalog, self.profile, catalog),
                ).fetchone()
                if legacy and not transfer:
                    raise CatabolicError(
                        "explicit transfer required from legacy automation"
                    )
                db.execute(
                    "INSERT OR REPLACE INTO watcher_owners VALUES (?,?,?)",
                    (self.profile, catalog, watcher),
                )
                db.execute(
                    "UPDATE fallback_bindings SET enabled=0 WHERE profile=? AND catalog=?",
                    (self.profile, catalog),
                )
                db.execute(
                    "UPDATE catalog_refresh_settings SET enabled=0 WHERE profile=? AND catalog=?",
                    (self.profile, catalog),
                )
            if not enabled:
                if row["active_run"]:
                    raise CatabolicError("watcher run active")
                db.execute(
                    "UPDATE watchers SET retry_at=0,requested_generation=completed_generation WHERE profile=? AND name=?",
                    (self.profile, watcher),
                )
                db.execute(
                    "DELETE FROM watcher_owners WHERE profile=? AND watcher=?",
                    (self.profile, watcher),
                )
            db.execute(
                "UPDATE watchers SET enabled=?,next_due=? WHERE profile=? AND name=?",
                (
                    int(enabled),
                    next_due(value["schedule"], time.time()) if enabled else 0,
                    self.profile,
                    watcher,
                ),
            )
        return self.get(watcher)

    def require_run(self, watcher, identifier, definition_id):
        current = self.get(watcher)
        if (
            current["active_run"] != identifier
            or current["definition_id"] != definition_id
            or current["lease_until"] <= time.time()
        ):
            raise CatabolicError("stale_watcher_run")

    def history(self, watcher):
        return self.store.rows(
            "SELECT * FROM watcher_runs WHERE profile=? AND watcher=? ORDER BY started_at DESC LIMIT 100",
            (self.profile, watcher),
        )

    def run(self, watcher, *, trigger="manual", now=None):
        now = time.time() if now is None else now
        row = self.get(watcher)
        value = row["definition"]
        if trigger == "scheduled":
            from .supervisor import due

            if not due({**row, "definition": encode(value)}, now):
                raise CatabolicError("watcher_not_due")
        if value["reaction"] == "projection":
            owners = self.store.rows(
                "SELECT watcher FROM watcher_owners WHERE profile=? AND catalog=?",
                (self.profile, value["plan"]["catalog"]),
            )
            if owners and owners[0]["watcher"] != watcher:
                raise CatabolicError("projection owned by another watcher")
        identifier = str(uuid4())
        with self.store.transaction() as db:
            if row["active_run"] and row["lease_until"] > now:
                raise CatabolicError("watcher_already_running")
            if row["active_run"]:
                db.execute(
                    "UPDATE watcher_runs SET state='interrupted',completed_at=? WHERE id=?",
                    (now, row["active_run"]),
                )
            generation = max(row["pending_generation"], row["completed_generation"] + 1)
            db.execute(
                "UPDATE watchers SET active_run=?,lease_until=?,pending_generation=? WHERE profile=? AND name=?",
                (identifier, now + 7200, generation, self.profile, watcher),
            )
            if trigger == "manual":
                db.execute(
                    "UPDATE watchers SET requested_generation=max(requested_generation,?) WHERE profile=? AND name=?",
                    (generation, self.profile, watcher),
                )
            db.execute(
                "INSERT INTO watcher_runs(id,profile,watcher,definition_id,generation,trigger,scheduled_at,started_at,state,worker_pid) VALUES (?,?,?,?,?,?,?,?,?,?)",
                (
                    identifier,
                    self.profile,
                    watcher,
                    row["definition_id"],
                    generation,
                    trigger,
                    row["next_due"] or now,
                    now,
                    "observing",
                    os.getpid(),
                ),
            )
        try:
            required = plans.observation_requirements(self.app, value["plan"])
            evidence = [
                observations.observe(
                    self.app,
                    r["source"],
                    max_age=value["max_age_seconds"],
                    after=now if value["observe_after_request"] else 0,
                    isolate=True,
                )
                for r in required
            ]
            with self.store.transaction() as db:
                db.execute(
                    "UPDATE watcher_runs SET result=? WHERE id=?",
                    (
                        encode(
                            {
                                "observations": evidence,
                                "complete": False,
                                "phase": "observing",
                            }
                        ),
                        identifier,
                    ),
                )
            if any(e["state"] in ("queued", "running") for e in evidence):
                raise CatabolicError("observation_pending")
            if value["require_complete_inventory"] and any(
                e["state"] != "complete" for e in evidence
            ):
                raise CatabolicError("complete_inventory_required")
            execute = (
                self._evaluate_owned
                if value["plan"]["kind"] in ("projection", "fallback")
                else self._complete_run
            )
            return execute(watcher, row, value, identifier, generation, now, evidence)
        except (CatabolicError, OSError, ValueError, sqlite3.Error) as exc:
            waiting = str(exc) == "observation_pending" or is_catalog_busy(exc)
            with self.store.transaction() as db:
                db.execute(
                    "UPDATE watcher_runs SET state=?,completed_at=?,error=? WHERE id=?",
                    (
                        "waiting" if waiting else "failed",
                        time.time(),
                        str(exc)[:4000],
                        identifier,
                    ),
                )
                db.execute(
                    "UPDATE watchers SET active_run=NULL,lease_until=0,retry_at=? WHERE profile=? AND name=? AND active_run=?",
                    (
                        time.time() + (1 if waiting else value["retry_seconds"]),
                        self.profile,
                        watcher,
                        identifier,
                    ),
                )
            return {"run_id": identifier, "complete": False, "error": str(exc)}

    @serialized
    def _evaluate_owned(self, *args):
        # Scans happen before this lock. One prepared projection retains ownership
        # across detached probes and publication; other reports can still run.
        return self._complete_run(*args)

    def _complete_run(self, watcher, row, value, identifier, generation, now, evidence):
        from .evaluation import EvaluationSession

        result = plans.evaluate(
            self.app,
            value["plan"],
            previous=json.loads(row["baseline"]) if row["baseline"] else None,
            context_name="watcher:" + watcher,
            session=EvaluationSession(
                self.store,
                self.profile,
                max_ids=100000,
                observations=[e["id"] for e in evidence],
            ),
        )
        comparison = plans.compare(
            json.loads(row["baseline"]) if row["baseline"] else None, result
        )
        self.require_run(watcher, identifier, row["definition_id"])
        # Persist admitted reaction before publication. A crash leaves durable demand.
        with self.store.transaction() as db:
            if result["kind"] == "fallback":
                db.execute(
                    "DELETE FROM fallback_health WHERE profile=? AND catalog=?",
                    (self.profile, "watcher:" + watcher),
                )
                db.executemany(
                    "INSERT INTO fallback_health VALUES (:profile,:catalog,:entry_id,:file_id,:revision,:healthy_since,:successes,:last_check)",
                    result["value"].get("health", []),
                )
            db.execute(
                "UPDATE watchers SET baseline=?,pending_reaction=? WHERE profile=? AND name=? AND active_run=?",
                (
                    encode(comparison["baseline"]),
                    identifier,
                    self.profile,
                    watcher,
                    identifier,
                ),
            )
            db.execute(
                "UPDATE watcher_runs SET state='reacting',result=? WHERE id=?",
                (
                    encode(
                        {
                            "observations": evidence,
                            "evaluation": comparison,
                            "plan_id": result["value"].get("plan_id")
                            if isinstance(result["value"], dict)
                            else None,
                            "complete": False,
                            "phase": "planned",
                            "retry_of": row["pending_reaction"],
                        }
                    ),
                    identifier,
                ),
            )
        self.require_run(watcher, identifier, row["definition_id"])
        reaction = None
        if value["reaction"] == "projection":
            reaction = FallbackProjection(self.app).apply_prepared(
                result["value"],
                max_changes=value["max_changes"],
                max_removals=value["max_removals"],
            )
            if not reaction.get("complete"):
                raise CatabolicError("projection_reaction_incomplete")
        elif value["reaction"] == "processing":
            from .watcher_processing import admit

            reaction = admit(self.app, value, result)
        elif value["reaction"] == "event" and (
            comparison["changed"] or row["pending_reaction"]
        ):
            from .notifications import emit

            with self.store.transaction() as db:
                emit(
                    db,
                    self.profile,
                    "watcher_changed",
                    "info",
                    watcher,
                    [identifier],
                )
        report = {
            "run_id": identifier,
            "definition_id": row["definition_id"],
            "observations": evidence,
            "evaluation": comparison,
            "reaction": reaction,
            "complete": True,
        }
        with self.store.transaction() as db:
            db.execute(
                "UPDATE watcher_runs SET state='complete',completed_at=?,result=? WHERE id=?",
                (time.time(), encode(report), identifier),
            )
            db.execute(
                "UPDATE watchers SET completed_generation=?,active_run=NULL,lease_until=0,pending_reaction=NULL,last_run=?,retry_at=0,next_due=?,event_first=CASE WHEN pending_generation<=? THEN NULL ELSE event_first END,event_last=CASE WHEN pending_generation<=? THEN NULL ELSE event_last END WHERE profile=? AND name=? AND active_run=?",
                (
                    generation,
                    identifier,
                    next_due(value["schedule"], now),
                    generation,
                    generation,
                    self.profile,
                    watcher,
                    identifier,
                ),
            )
        return report


def dirty(db, profile):
    # Conservative catalog event invalidation; schedule admission remains separate.
    db.execute(
        "UPDATE watchers SET pending_generation=pending_generation+1,event_first=coalesce(event_first,unixepoch('now')),event_last=unixepoch('now') WHERE profile=? AND enabled=1 AND definition_id IN (SELECT id FROM watcher_definitions WHERE json_extract(definition,'$.events')=1)",
        (profile,),
    )
