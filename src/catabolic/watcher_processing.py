# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Explicit processing reaction; pin resolved inputs before existing admission."""

from .artifacts import Artifacts
from .content_access import revision_of
from .domain import CatabolicError
from .operations import Operations
from .processing import Processing


def admit(app, definition, result):
    operation = Operations(app).get(definition["operation_id"])
    if operation["operation_kind"] not in ("render", "analysis"):
        raise CatabolicError("watcher operation kind is not supported")
    lineage_mode = None
    if result["kind"] == "fallback":
        from .fallback_policies import Policies

        lineage_mode = Policies(app.store, app.profile).get(
            result["value"]["policy_id"]
        )["definition"]["lineage_requirement"]
        inputs = [
            {
                "file_id": d["file_id"],
                "item_id": d["item_id"],
                "revision": d["revision"],
            }
            for d in result["value"]["decisions"]
            if d["state"].startswith("resolved")
        ]
        if len(inputs) != len(result["value"]["decisions"]):
            raise CatabolicError("processing fallback has unresolved inputs")
    elif result["kind"] == "selection" and result["value"]["entity"] == "file_id":
        inputs = []
        for file_id in result["value"]["ids"]:
            associations = app.store.rows(
                "SELECT DISTINCT item_id FROM item_files WHERE file_id=? AND active=1 AND role='primary' LIMIT 2",
                (file_id,),
            )
            if len(associations) != 1:
                raise CatabolicError(
                    "processing requires unambiguous primary item association"
                )
            inputs.append(
                {
                    "file_id": file_id,
                    "item_id": associations[0]["item_id"],
                    "revision": revision_of(app.store, app.profile, file_id),
                }
            )
    else:
        raise CatabolicError(
            "processing requires complete file IDs or resolved fallback inputs"
        )
    if len(inputs) > definition["max_jobs"]:
        raise CatabolicError("watcher_processing_budget")
    capabilities = None
    if operation["operation_kind"] == "render":
        if not definition["destination"]:
            raise CatabolicError("render reaction requires an approved destination")
        from .rendering import capabilities as available

        with app.store.detached():
            capabilities = available()
    jobs = []
    for row in inputs:
        if revision_of(app.store, app.profile, row["file_id"]) != row["revision"]:
            raise CatabolicError("stale_watcher_processing_input")
        if operation["operation_kind"] == "render":
            job = Artifacts(app).enqueue(
                row["file_id"],
                operation["id"],
                definition["destination"],
                row["item_id"],
                _capabilities=capabilities,
                _full_cache_check=False,
                _source_lineage_mode=lineage_mode,
            )
        else:
            job = Processing(app).enqueue(
                operation["preset"],
                file_ids=[row["file_id"]],
                options=operation["definition"]["options"],
                recipe_id=operation["id"],
            )
            if job["errors"]:
                raise CatabolicError("watcher_processing_admission_failed")
        jobs.append({"input": row, "admission": job})
    return {"jobs": jobs, "complete": True}
