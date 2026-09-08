# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Saved selections driving existing artifact jobs, with explicit space previews."""

import hashlib
import json
import os
from collections import Counter
from uuid import uuid4

from .artifacts import Artifacts
from .curation import bounded_rows, occurrence, page_limit
from .domain import CatabolicError, name
from .estimate_calibration import calibrated, calibration
from .filesystem import owner_state, root_handle
from .item_workflow import ItemWorkflow
from .operations import Operations
from .processing import Processing, current_fact
from .rule_estimates import estimate, human_bytes, total
from .rule_estimates import options as estimate_options
from .selection import selected_associations, validate_selection
from .source_access import validated_source
from .store import encode

STATES = ("missing", "stale", "satisfied", "queued", "running", "failed", "deferred")


class Rules:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile
        self.artifacts = Artifacts(app)

    @staticmethod
    def decode(row):
        return {
            **row,
            "selection": json.loads(row["selection"]),
            "estimate_options": json.loads(row["estimate_options"]),
            "enabled": bool(row["enabled"]),
            "required": bool(row["required"]),
        }

    def get(self, identifier):
        rows = self.store.rows(
            "SELECT * FROM processing_rules WHERE id=? AND profile=?",
            (identifier, self.profile),
        )
        if not rows:
            raise CatabolicError("unknown rule revision in this profile")
        return self.decode(rows[0])

    def list(self, *, limit=100, after="", enabled=False):
        page_limit(limit)
        rows, more = bounded_rows(
            self.store,
            "SELECT * FROM processing_rules WHERE profile=? AND id>? AND (?=0 OR enabled=1) ORDER BY id LIMIT ?",
            (self.profile, after, int(enabled), limit + 1),
            limit,
        )
        return {
            "rules": [self.decode(row) for row in rows],
            "next_after": rows[-1]["id"] if more else None,
        }

    def put(
        self,
        rule_name,
        recipe_id,
        location,
        selection,
        *,
        estimates=None,
        required=False,
        allow_derived=False,
    ):
        self.app.require_recovered()
        name(rule_name)
        operation = Operations(self.app).get(recipe_id)
        if not isinstance(selection, dict):
            raise CatabolicError("selection must be a SQL/GraphQL selection object")
        selection = validate_selection({"profile": self.profile, **selection})
        if "query_id" in selection:
            from .saved_queries import Queries

            if (
                Queries(self.store, self.profile).get(selection["query_id"])[
                    "definition"
                ]["mode"]
                != "selection"
            ):
                raise CatabolicError("rule requires a complete ID selection")
        if selection.get("profile", "default") != self.profile:
            raise CatabolicError("rule selection must use the rule's profile")
        if operation["operation_kind"] != "render" and location is not None:
            raise CatabolicError(
                "analysis and external rules do not take a local generated destination"
            )
        if operation["operation_kind"] == "render" and not self.store.rows(
            "SELECT 1 FROM generated_locations WHERE profile=? AND location=?",
            (self.profile, location),
        ):
            raise CatabolicError("rules require a bound generated destination")
        assumptions = estimate_options({} if estimates is None else estimates)
        if type(allow_derived) is not bool:
            raise CatabolicError("allow_derived must be boolean")
        value = encode(
            [recipe_id, location, selection, assumptions, bool(required)]
            + ([True] if allow_derived else [])
        )
        digest = hashlib.sha256(value.encode()).hexdigest()
        with self.store.transaction() as db:
            old = db.execute(
                "SELECT id FROM processing_rules WHERE profile=? AND name=? AND digest=?",
                (self.profile, rule_name, digest),
            ).fetchone()
            if old:
                identifier = old[0]
            else:
                revision = db.execute(
                    "SELECT coalesce(max(revision),0)+1 FROM processing_rules WHERE profile=? AND name=?",
                    (self.profile, rule_name),
                ).fetchone()[0]
                identifier = str(uuid4())
                db.execute(
                    "UPDATE processing_rules SET enabled=0 WHERE profile=? AND name=?",
                    (self.profile, rule_name),
                )
                db.execute(
                    "INSERT INTO processing_rules(id,profile,name,revision,recipe_id,location,selection,estimate_options,required,digest,query_id,allow_derived) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
                    (
                        identifier,
                        self.profile,
                        rule_name,
                        revision,
                        recipe_id,
                        location,
                        encode(selection),
                        encode(assumptions),
                        int(required),
                        digest,
                        selection.get("query_id"),
                        int(allow_derived),
                    ),
                )
        return self.get(identifier)

    def enable(self, identifier, enabled):
        rule = self.get(identifier)
        with self.store.transaction() as db:
            if enabled:
                db.execute(
                    "UPDATE processing_rules SET enabled=0 WHERE profile=? AND name=?",
                    (self.profile, rule["name"]),
                )
            db.execute(
                "UPDATE processing_rules SET enabled=? WHERE id=?",
                (int(enabled), identifier),
            )
        return self.get(identifier)

    def _candidate(self, rule, recipe, binding, file_id, item_id):
        row = {
            "file_id": file_id,
            "item_id": item_id,
            "state": "missing",
            "reason": None,
            "job_id": None,
            "existing_bytes": 0,
        }
        observation = occurrence(self.store, self.profile, file_id)
        fact = current_fact(self.store, self.profile, file_id)
        probe = fact["data"] if fact and fact["current"] else None
        row["estimate"] = estimate(
            recipe["definition"], observation, probe, rule["estimate_options"]
        )
        try:
            from .outputs import RENDITIONS_SQL
            from .rendition_publication import ready

            for output in self.store.rows(
                f"SELECT * FROM ({RENDITIONS_SQL}) WHERE file_id=? AND profile=?",
                (file_id, self.profile),
            ):
                ready(self.app, output)
            with validated_source(observation) as fd:
                current_ctime = os.fstat(fd).st_ctime_ns
        except (CatabolicError, OSError) as exc:
            return {**row, "state": "deferred", "reason": str(exc)}
        if fact and fact["snapshot"].get("ctime_ns") != current_ctime:
            probe = None
            row["estimate"] = estimate(
                recipe["definition"], observation, None, rule["estimate_options"]
            )
        jobs = self.store.rows(
            "SELECT * FROM processing_jobs WHERE profile=? AND file_id=? AND operation='render' AND recipe_id=? AND json_extract(options,'$.item_id')=? AND json_extract(options,'$.destination.owner')=? ORDER BY created_at DESC,rowid DESC LIMIT 1001",
            (self.profile, file_id, rule["recipe_id"], item_id, rule["location"]),
        )
        if len(jobs) > 1000:
            raise CatabolicError(
                "more than 1000 historical jobs for one rule input; inspect before planning"
            )
        for job in jobs:
            snapshot, config = json.loads(job["snapshot"]), json.loads(job["options"])
            current = (
                all(
                    observation.get(k) == v
                    for k, v in snapshot.items()
                    if k != "ctime_ns"
                )
                and snapshot.get("ctime_ns") == current_ctime
                and config["destination"] == binding
            )
            if not current:
                row["state"] = "stale"
                continue
            row["job_id"] = job["id"]
            if job["state"] in ("queued", "running"):
                return {**row, "state": job["state"]}
            if job["state"] != "complete":
                return {
                    **row,
                    "state": "failed",
                    "reason": "Prior job failed or was cancelled; use --retry-failed explicitly.",
                }
            ready = self.store.rows(
                "SELECT * FROM processing_artifacts WHERE job_id=? AND state='ready'",
                (job["id"],),
            )
            if ready:
                artifact = ready[0]
                out = occurrence(self.store, self.profile, artifact["file_id"])
                publication = json.loads(artifact["publication_snapshot"] or "{}")
                try:
                    published_current = bool(publication) and not any(
                        out.get(k) != v
                        for k, v in publication.items()
                        if k != "ctime_ns"
                    )
                    verified = current_fact(
                        self.store, self.profile, artifact["file_id"], "verify"
                    )
                    verified_current = (
                        verified
                        and verified["current"]
                        and verified["data"].get("digest") == artifact["sha256"]
                    )
                    if not published_current and not verified_current:
                        raise CatabolicError("output changed since publication")
                    with validated_source(out):
                        pass
                    return {
                        **row,
                        "state": "satisfied",
                        "existing_bytes": artifact["size"],
                        "artifact_id": artifact["id"],
                    }
                except (CatabolicError, OSError):
                    pass
            row["state"] = "stale"
            break
        if not probe:
            return {
                **row,
                "state": "deferred",
                "reason": "Current probe facts are required; enqueue probe work first.",
            }
        preset = recipe["preset"]
        expected = {
            "thumbnail": "video",
            "preview": "video",
            "h264-720p": "video",
            "h264-1080p": "video",
            "audio-aac": "audio",
            "audio-flac": "audio",
            "subtitle-srt": "subtitle",
        }.get(preset)
        streams = [
            s for s in probe.get("streams", []) if s.get("codec_type") == expected
        ]
        index = (
            recipe["definition"].get("video_stream", 0)
            if expected == "video"
            else recipe["definition"].get("stream", 0)
        )
        if expected and len(streams) <= index:
            return {
                **row,
                "state": "deferred",
                "reason": "Required stream is absent in recorded probe facts.",
            }
        if preset in ("preview", "h264-720p", "h264-1080p") and probe.get(
            "summary", {}
        ).get("hdr"):
            return {
                **row,
                "state": "deferred",
                "reason": "Current transcode presets support SDR; HDR needs a reviewed tone-mapping recipe.",
            }
        return row

    def _plan(self, identifier, scan_ids=None):
        rule = self.get(identifier)
        recipe = Operations(self.app).get(rule["recipe_id"])
        if recipe["operation_kind"] == "external":
            from .rule_external import ExternalRule

            return ExternalRule(self).plan(rule, recipe, scan_ids)
        if recipe["operation_kind"] == "analysis":
            from .rule_analysis import plan

            return plan(self, rule, recipe, scan_ids)
        binding = self.app.binding("source", rule["location"])
        with root_handle(binding) as fd:
            if owner_state(fd, self.artifacts._owner(rule["location"])) != "owned":
                raise CatabolicError("generated destination ownership is missing")
            space = os.fstatvfs(fd)
            available = space.f_bavail * space.f_frsize
        admitted = []
        if rule["allow_derived"]:
            from .selection import select_ids

            entity, identifiers, _ = select_ids(self.store, rule["selection"])
            column = "a.id" if entity == "association_id" else "a." + entity
            identifiers = sorted(identifiers)
            for start in range(0, len(identifiers), 500):
                group = identifiers[start : start + 500]
                admitted.extend(
                    self.store.rows(
                        f"SELECT a.*,f.path,f.location FROM item_files a JOIN files f ON f.id=a.file_id WHERE {column} IN ({','.join('?' for _ in group)}) AND EXISTS (SELECT 1 FROM media_outputs m WHERE m.file_id=a.file_id AND m.item_id=a.item_id AND m.profile=?) LIMIT ?",
                        (*group, self.profile, 100001 - len(admitted)),
                    )
                )
                if len(admitted) > 100000:
                    raise CatabolicError(
                        "derived selection expands beyond 100000 associations"
                    )
        associations, selection = selected_associations(
            self.store, rule["selection"], admitted=admitted
        )
        derived = set()
        ids = sorted({a["file_id"] for a in associations})
        for start in range(0, len(ids), 500):
            group = ids[start : start + 500]
            marks = ",".join("?" for _ in group)
            derived.update(
                r["id"]
                for r in self.store.rows(
                    f"SELECT f.id FROM files f WHERE f.id IN ({marks}) AND (EXISTS (SELECT 1 FROM media_outputs m WHERE m.file_id=f.id) OR EXISTS (SELECT 1 FROM generated_locations g WHERE g.location=f.location))",
                    tuple(group),
                )
            )
        pairs = sorted(
            {
                (a["file_id"], a["item_id"])
                for a in associations
                if (
                    a["role"] == "primary"
                    or selection["entity"] in ("file_id", "association_id")
                    or rule["allow_derived"]
                    and a["file_id"] in derived
                )
                and (rule["allow_derived"] or a["file_id"] not in derived)
            }
        )
        if len(pairs) > 100000:
            raise CatabolicError(
                "rule expands beyond 100000 inputs; narrow the saved selection"
            )
        rows = [self._candidate(rule, recipe, binding, f, i) for f, i in pairs]
        measured = calibration(
            self.store, self.profile, recipe, rule["estimate_options"]
        )
        for row in rows:
            row["estimate"] = calibrated(row["estimate"], measured)
        if scan_ids is not None:
            for row in rows:
                seen = self.store.rows(
                    "SELECT scan_id FROM observations WHERE profile=? AND file_id=?",
                    (self.profile, row["file_id"]),
                )
                if row["state"] != "satisfied" and (
                    not seen or seen[0]["scan_id"] not in scan_ids
                ):
                    row.update(
                        state="deferred",
                        reason="Input was outside the successful maintenance scan scope.",
                    )
        counts = dict.fromkeys(STATES, 0)
        counts.update(Counter(row["state"] for row in rows))
        estimate_total = total(
            row["estimate"] for row in rows if row["state"] != "satisfied"
        )
        estimate_total.update(
            {
                "already_satisfied_bytes": sum(row["existing_bytes"] for row in rows),
                "available_bytes": available,
                "destination": binding["root"],
                "device": binding["device"],
                "reserve_bytes": recipe["definition"]["reserve_bytes"],
                "fits_planning_estimate": available
                >= estimate_total["planning_bytes"]
                + recipe["definition"]["reserve_bytes"],
                "note": "Additional logical storage, retaining originals and old outputs. Unknown sizes use recipe limits for planning. Ranges are heuristics, not guaranteed bounds; no sample encode or full-file read was performed.",
            }
        )
        return {
            "rule": rule,
            "selection": selection,
            "complete": True,
            "matched_inputs": len(rows),
            "excluded_associations": len(associations) - len(pairs),
            "counts": counts,
            "space": estimate_total,
            "matches": rows,
            "calibration": measured,
        }

    @staticmethod
    def _bounded(plan, limit):
        page_limit(limit)
        return {
            **plan,
            "matches": plan["matches"][:limit],
            "details_truncated": len(plan["matches"]) > limit,
        }

    def preview(self, identifier, *, limit=100):
        page_limit(limit)
        return self._bounded(self._plan(identifier), limit)

    def _attach(self, rule, row, job_id):
        if not self.store.rows(
            "SELECT 1 FROM rule_jobs WHERE rule_id=? AND job_id=?", (rule["id"], job_id)
        ):
            with self.store.transaction() as db:
                db.execute(
                    "INSERT INTO rule_jobs(rule_id,job_id,file_id,item_id) VALUES (?,?,?,?) ON CONFLICT DO NOTHING",
                    (rule["id"], job_id, row["file_id"], row["item_id"]),
                )
        if rule["required"]:
            with self.store.transaction() as db:
                old = db.execute(
                    "SELECT * FROM rule_requirements WHERE rule_id=? AND file_id=? AND item_id=?",
                    (rule["id"], row["file_id"], row["item_id"]),
                ).fetchone()
                if old and old["job_id"] != job_id:
                    db.execute(
                        "UPDATE rule_requirements SET job_id=? WHERE id=?",
                        (job_id, old["id"]),
                    )
                    workflow = ItemWorkflow(self.app)
                    status, revision = workflow._revision(db, row["item_id"], None)
                    workflow._append(
                        db,
                        row["item_id"],
                        "requirement",
                        "Rule requirement now tracks this job; prior attempts remain in history.",
                        "rule:" + rule["id"],
                        {
                            "requirement_id": old["id"],
                            "previous_job_id": old["job_id"],
                            "job_id": job_id,
                        },
                        status,
                        revision,
                    )

    def _requirements(self, plan):
        rule = plan["rule"]
        if not rule["required"]:
            return
        evaluation = str(uuid4())
        workflow = ItemWorkflow(self.app)
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO rule_evaluations(id,rule_id,matched_inputs,counts) VALUES (?,?,?,?)",
                (
                    evaluation,
                    rule["id"],
                    plan["matched_inputs"],
                    encode(plan["counts"]),
                ),
            )
            for row in plan["matches"]:
                old = db.execute(
                    "SELECT id FROM rule_requirements WHERE rule_id=? AND file_id=? AND item_id=?",
                    (rule["id"], row["file_id"], row["item_id"]),
                ).fetchone()
                if old:
                    db.execute(
                        "UPDATE rule_requirements SET evaluation_id=? WHERE id=?",
                        (evaluation, old["id"]),
                    )
                    continue
                identifier = str(uuid4())
                db.execute(
                    "INSERT INTO rule_requirements(id,rule_id,profile,file_id,item_id,evaluation_id) VALUES (?,?,?,?,?,?)",
                    (
                        identifier,
                        rule["id"],
                        self.profile,
                        row["file_id"],
                        row["item_id"],
                        evaluation,
                    ),
                )
                status, revision = workflow._revision(db, row["item_id"], None)
                workflow._append(
                    db,
                    row["item_id"],
                    "requirement",
                    "A current operation result is required, including work not yet queued.",
                    "rule:" + rule["id"],
                    {
                        "requirement_id": identifier,
                        "rule_id": rule["id"],
                        "file_id": row["file_id"],
                    },
                    status,
                    revision,
                )

    def statistics(self, identifier):
        rule = self.get(identifier)
        operation = Operations(self.app).get(rule["recipe_id"])
        if operation["operation_kind"] == "external":
            return {
                "rule_id": identifier,
                "calibration": None,
                "attempts": self.store.rows(
                    "SELECT a.state,count(*) AS count,sum(CASE WHEN a.finished_at IS NOT NULL THEN max(0,a.finished_at-a.started_at) END) AS lease_elapsed_seconds FROM rule_processor_jobs r JOIN processor_attempts a ON a.job_id=r.job_id WHERE r.rule_id=? GROUP BY a.state ORDER BY a.state",
                    (identifier,),
                ),
                "note": "Historical external lease attempts; lease duration is not measured processor runtime. Output storage and runtime remain unknown.",
            }
        return {
            "rule_id": identifier,
            "calibration": calibration(
                self.store,
                self.profile,
                operation,
                rule["estimate_options"],
            )
            if operation["operation_kind"] == "render"
            else None,
            "attempts": self.store.rows(
                """SELECT p.state,count(*) AS count,sum(a.size) AS output_bytes,
 sum(CASE WHEN p.started_at IS NOT NULL AND p.finished_at IS NOT NULL THEN max(0,(julianday(p.finished_at)-julianday(p.started_at))*86400) END) AS elapsed_seconds
 FROM rule_jobs r JOIN processing_attempts p ON p.job_id=r.job_id
 LEFT JOIN processing_artifacts a ON a.job_id=p.job_id AND a.attempt=p.attempt
 WHERE r.rule_id=? GROUP BY p.state ORDER BY p.state""",
                (identifier,),
            ),
            "note": "Historical attempt totals, including retained old outputs. Elapsed times are recorded wall time; no savings or future ETA is inferred.",
        }

    def apply(
        self,
        identifier,
        *,
        batch=100,
        limit=100,
        max_new_bytes=None,
        retry_failed=False,
        scan_ids=None,
    ):
        page_limit(batch)
        page_limit(limit)
        if max_new_bytes is not None and (
            type(max_new_bytes) is not int or max_new_bytes < 0
        ):
            raise CatabolicError("max new bytes must be a nonnegative integer")
        self.app.require_recovered()
        plan = self._plan(identifier, scan_ids)
        rule = plan["rule"]
        operation = Operations(self.app).get(rule["recipe_id"])
        if operation["operation_kind"] == "external":
            from .rule_external import ExternalRule

            return ExternalRule(self).apply(
                plan, operation, batch, limit, retry_failed, max_new_bytes
            )
        candidates = [
            r
            for r in plan["matches"]
            if r["state"] in ("missing", "stale")
            or retry_failed
            and r["state"] == "failed"
        ]
        selected = candidates[:batch]
        storage = total(row["estimate"] for row in selected)
        blockers = []
        if (
            selected
            and plan["space"]["available_bytes"] is not None
            and storage["planning_bytes"] + plan["space"]["reserve_bytes"]
            > plan["space"]["available_bytes"]
        ):
            blockers.append(
                "Selected batch exceeds available destination space using the planning estimate."
            )
        if max_new_bytes is not None and storage["planning_bytes"] > max_new_bytes:
            blockers.append(
                "Selected batch exceeds --max-new-bytes using the planning estimate."
            )
        result = {
            **self._bounded(plan, limit),
            "batch_space": storage,
            "queued": [],
            "adopted": 0,
            "errors": [],
            "blockers": blockers,
            "remaining_to_enqueue": len(candidates),
            "safe": not blockers,
            "complete": False,
        }
        if blockers:
            return result
        self._requirements(plan)
        # Adopt matching jobs from an earlier explicit enqueue or interrupted rule application.
        for row in plan["matches"]:
            if row["state"] in ("satisfied", "queued", "running") and row["job_id"]:
                self._attach(rule, row, row["job_id"])
                result["adopted"] += 1
        capabilities = None
        for row in selected:
            try:
                operation = Operations(self.app).get(rule["recipe_id"])
                if operation["operation_kind"] == "analysis":
                    processing = Processing(self.app)
                    if row["state"] == "failed" and row["job_id"]:
                        processing.retry(row["job_id"])
                        queued = {"job_id": row["job_id"]}
                    else:
                        outcome = processing.enqueue(
                            operation["preset"],
                            file_ids=[row["file_id"]],
                            options=operation["definition"]["options"],
                            recipe_id=operation["id"],
                            refresh=row["state"] == "stale",
                        )
                        if outcome["errors"]:
                            raise CatabolicError(outcome["errors"][0]["error"])
                        queued = {"job_id": (outcome["queued"] + outcome["cached"])[0]}
                else:
                    if capabilities is None:
                        from .rendering import capabilities as available_capabilities

                        capabilities = available_capabilities()
                    queued = self.artifacts.enqueue(
                        row["file_id"],
                        rule["recipe_id"],
                        rule["location"],
                        row["item_id"],
                        _capabilities=capabilities,
                    )
                if row["state"] == "stale" and queued.get("artifact_id"):
                    raise CatabolicError(
                        "Existing output checksum is intact but its recorded evidence is stale; scan the generated location and run process enqueue verify for the artifact file before reapplying this rule."
                    )
                self._attach(rule, row, queued["job_id"])
                result["queued"].append(queued)
            except (OSError, CatabolicError) as exc:
                result["errors"].append(
                    {
                        "file_id": row["file_id"],
                        "item_id": row["item_id"],
                        "message": str(exc),
                    }
                )
                break
        result["remaining_to_enqueue"] -= len(result["queued"])
        result["complete"] = (
            not result["errors"]
            and result["remaining_to_enqueue"] == 0
            and not plan["counts"]["deferred"]
            and (retry_failed or not plan["counts"]["failed"])
        )
        return result

    def run(self, identifier, *, batch=1, limit=100, scan_ids=None, _refresh=True):
        page_limit(batch)
        page_limit(limit)
        plan = self._plan(identifier, scan_ids)
        if (
            Operations(self.app).get(plan["rule"]["recipe_id"])["operation_kind"]
            == "external"
        ):
            return {
                "rule_id": identifier,
                "execution": {
                    "completed": [],
                    "errors": [],
                    "complete": True,
                    "executor": "processor tick or leased processor dispatch",
                    "pending_external": plan["counts"]["queued"]
                    + plan["counts"]["running"],
                },
                "after": self._bounded(plan, limit),
                "complete": plan["counts"]["satisfied"] == plan["matched_inputs"],
            }
        registered = {
            r["job_id"]
            for r in self.store.rows(
                "SELECT job_id FROM rule_jobs WHERE rule_id=?", (identifier,)
            )
        }
        selected = [
            r["job_id"]
            for r in plan["matches"]
            if r["state"] == "queued" and r["job_id"] in registered
        ][:batch]
        batch_space = total(
            row["estimate"] for row in plan["matches"] if row["job_id"] in selected
        )
        if (
            selected
            and plan["space"]["available_bytes"] is not None
            and batch_space["planning_bytes"] + plan["space"]["reserve_bytes"]
            > plan["space"]["available_bytes"]
        ):
            result = {
                "completed": [],
                "errors": [
                    {
                        "error": "Render batch exceeds available space using the planning estimate."
                    }
                ],
                "complete": False,
            }
        else:
            operation = Operations(self.app).get(plan["rule"]["recipe_id"])
            executor = (
                Processing(self.app)
                if operation["operation_kind"] == "analysis"
                else self.artifacts
            )
            result = executor.run(
                limit=batch, job_ids=list(dict.fromkeys(selected)), _refresh=_refresh
            )
        after = self._bounded(self._plan(identifier, scan_ids), limit)
        return {
            "rule_id": identifier,
            "execution": result,
            "after": after,
            "complete": result["complete"]
            and after["counts"]["satisfied"] == after["matched_inputs"],
        }


def maintain(
    app, *, batch=100, render_batch=0, max_new_bytes=None, limit=100, scan_ids=None
):
    rules = Rules(app)
    enabled = app.store.rows(
        "SELECT id FROM processing_rules WHERE profile=? AND enabled=1 ORDER BY name,id LIMIT 1001",
        (app.profile,),
    )
    if len(enabled) > 1000:
        raise CatabolicError("maintenance supports at most 1000 enabled rules")
    plans = [rules._plan(row["id"], scan_ids) for row in enabled]
    volumes = {}
    seen = set()
    for plan in plans:
        space, rule = plan["space"], plan["rule"]
        if space["device"] is None:
            continue
        group = volumes.setdefault(
            space["device"],
            {
                "device": space["device"],
                "available_bytes": space["available_bytes"],
                "estimates": [],
                "reserve_bytes": 0,
                "queued_estimates": [],
            },
        )
        group["reserve_bytes"] = max(group["reserve_bytes"], space["reserve_bytes"])
        for row in plan["matches"]:
            key = (row["file_id"], row["item_id"], rule["recipe_id"], rule["location"])
            if key not in seen and row["state"] != "satisfied":
                group["estimates"].append(row["estimate"])
                if row["state"] in ("queued", "running"):
                    group["queued_estimates"].append(row["estimate"])
                seen.add(key)
    volume_reports = [
        {
            **{
                k: v
                for k, v in group.items()
                if k not in ("estimates", "queued_estimates")
            },
            **total(group["estimates"]),
        }
        for group in volumes.values()
    ]
    results = []
    remaining, render_remaining, bytes_remaining = batch, render_batch, max_new_bytes
    healthy = True
    allowances = {
        device: max(
            0,
            group["available_bytes"]
            - group["reserve_bytes"]
            - total(group["queued_estimates"])["planning_bytes"],
        )
        for device, group in volumes.items()
    }
    # Budget applies across enabled rules. Explicit per-rule apply/run is available
    # when a large earlier rule should not consume the maintenance batch.
    for plan in plans:
        identifier = plan["rule"]["id"]
        result = {"rule_id": identifier, "preview": rules._bounded(plan, limit)}
        if remaining:
            allowance = allowances.get(plan["space"]["device"])
            if bytes_remaining is not None:
                allowance = (
                    bytes_remaining
                    if allowance is None
                    else min(allowance, bytes_remaining)
                )
            applied = rules.apply(
                identifier,
                batch=remaining,
                max_new_bytes=allowance,
                limit=limit,
                scan_ids=scan_ids,
            )
            result["apply"] = applied
            remaining -= len(applied["queued"])
            if bytes_remaining is not None and applied["safe"]:
                bytes_remaining -= applied["batch_space"]["planning_bytes"]
            if applied["safe"] and plan["space"]["device"] is not None:
                allowances[plan["space"]["device"]] -= applied["batch_space"][
                    "planning_bytes"
                ]
            if applied["errors"] or not applied["safe"]:
                healthy = False
        if render_remaining and healthy:
            executed = rules.run(
                identifier,
                batch=render_remaining,
                limit=limit,
                scan_ids=scan_ids,
                _refresh=False,
            )
            result["run"] = executed
            render_remaining -= len(executed["execution"]["completed"]) + len(
                executed["execution"]["errors"]
            )
            healthy = executed["execution"]["complete"]
        results.append(result)
        if not healthy:
            break
    after = [rules._bounded(rules._plan(row["id"], scan_ids), limit) for row in enabled]
    counts = dict.fromkeys(STATES, 0)
    for plan in after:
        for key, value in plan["counts"].items():
            counts[key] += value
    return {
        "rules": results,
        "backlog": counts,
        "space_by_device": volume_reports,
        "space_scope": "All outstanding enabled-rule matches before this cycle, deduplicated by input/item/recipe/destination.",
        "safe_to_continue": healthy,
        "complete": healthy
        and all(p["counts"]["satisfied"] == p["matched_inputs"] for p in after),
    }


def render_preview(result):
    execution_errors = result.get("execution", {}).get("errors", [])
    refresh = result.get("execution", {}).get("catalog_refresh")
    if "after" in result:
        result = result["after"]
    rule, space = result["rule"], result["space"]
    lines = [
        f"Rule: {rule['name']} revision {rule['revision']} ({rule['id']})",
        f"Matched inputs: {result['matched_inputs']}",
        "; ".join(f"{k}: {v}" for k, v in result["counts"].items()),
        "Additional output space: " + human_bytes(space["expected_bytes"]),
        f"Estimated range: {human_bytes(space['low_bytes'])} to {human_bytes(space['high_bytes'])}",
        f"Unknown output sizes: {space['unknown_count']}; known subtotal: {human_bytes(space['known_expected_bytes'])}",
        "Existing satisfied outputs: " + human_bytes(space["already_satisfied_bytes"]),
        "Available destination space: " + human_bytes(space["available_bytes"]),
        "Planning allowance including unknown recipe limits: "
        + human_bytes(space["planning_bytes"]),
        space["note"],
    ]
    if "batch_space" in result:
        lines += [
            "Selected batch allowance: "
            + human_bytes(result["batch_space"]["planning_bytes"]),
            f"Jobs queued/reused: {len(result['queued'])}; still to enqueue: {result['remaining_to_enqueue']}",
        ]
        lines.extend(result["blockers"])
        lines.extend(error["message"] for error in result["errors"])
    for row in result["matches"]:
        lines.append(
            f"  {row['file_id']} / {row['item_id']}: {row['state']}; {human_bytes(row['estimate']['expected_bytes'])}"
            + ("; " + row["reason"] if row["reason"] else "")
        )
        lines.extend("    " + value for value in row["estimate"]["assumptions"])
    lines.extend("Rendering error: " + error["error"] for error in execution_errors)
    if refresh:
        lines.append(
            "Catalog link refresh: "
            + ("complete" if refresh["complete"] else "pending retry")
        )
        lines.extend(
            "Link refresh: " + error["error"] for error in refresh.get("errors", [])
        )
    return "\n".join(lines)
