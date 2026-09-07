# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Bounded, idempotent imports of externally delivered rendition receipts."""

import hashlib
import json
import os
import re
import time
from contextlib import ExitStack
from uuid import uuid4

from .curation import occurrence
from .domain import CatabolicError, relative_path
from .item_workflow import text
from .outputs import Outputs
from .source_access import validated_source
from .store import encode

REVISION_FIELDS = ("size", "mtime_ns", "device", "inode", "ctime_ns")


def source_receipt(app, file_id, item_id):
    Outputs(app).validate_source_item(file_id, item_id, {"mode": "same_item"})
    snapshot = occurrence(app.store, app.profile, file_id)
    with validated_source(snapshot) as fd:
        snapshot["ctime_ns"] = os.fstat(fd).st_ctime_ns
    return {
        "database_id": app.store.database_id,
        "profile": app.profile,
        "source": {
            "file_id": file_id,
            "item_id": item_id,
            "revision": {k: snapshot[k] for k in REVISION_FIELDS},
        },
    }


def import_receipt(app, value, *, _accepted=None):
    app.require_recovered()
    keys = {
        "version",
        "database_id",
        "profile",
        "producer",
        "instance",
        "job_id",
        "attempt",
        "configuration_digest",
        "tools",
        "outcome",
        "source",
        "outputs",
    }
    if not isinstance(value, dict) or set(value) != keys:
        raise CatabolicError("receipt must contain exactly the documented fields")
    if (
        type(value["version"]) is not int
        or value["version"] != 1
        or value["outcome"] != "complete"
    ):
        raise CatabolicError("receipt must be version 1 with a complete outcome")
    if value["database_id"] != app.store.database_id or value["profile"] != app.profile:
        raise CatabolicError("receipt belongs to another database or profile")
    for key in ("producer", "instance", "job_id", "attempt"):
        text(value[key], "receipt " + key, 255, empty=False)
    if not isinstance(value["configuration_digest"], str) or not re.fullmatch(
        "[0-9a-f]{64}", value["configuration_digest"]
    ):
        raise CatabolicError("configuration_digest must be a lowercase SHA-256")
    if (
        not isinstance(value["tools"], dict)
        or not value["tools"]
        or len(value["tools"]) > 32
    ):
        raise CatabolicError("receipt tools must declare 1..32 tool versions")
    for key, version in value["tools"].items():
        text(key, "tool name", 255, empty=False)
        text(version, "tool version", 4096, empty=False)
    try:
        payload = json.dumps(
            value, sort_keys=True, separators=(",", ":"), allow_nan=False
        )
    except (ValueError, TypeError, RecursionError) as exc:
        raise CatabolicError("receipt must contain finite JSON values") from exc
    if len(payload.encode()) > 1048576:
        raise CatabolicError("receipt exceeds 1 MiB")
    digest = hashlib.sha256(payload.encode()).hexdigest()
    identity = (
        app.profile,
        *(value[k] for k in ("producer", "instance", "job_id", "attempt")),
    )
    previous = app.store.rows(
        "SELECT id,digest FROM external_receipts WHERE profile=? AND producer=? AND instance=? AND job_id=? AND attempt=?",
        identity,
    )
    if previous:
        if previous[0]["digest"] != digest:
            raise CatabolicError("receipt identity already has a conflicting payload")
        if _accepted is not None:
            with app.store.transaction() as db:
                _accepted(db, previous[0]["id"])
        return {
            "receipt_id": previous[0]["id"],
            "reused": True,
            "output_ids": [
                r["output_id"]
                for r in app.store.rows(
                    "SELECT output_id FROM receipt_outputs WHERE receipt_id=? ORDER BY output_id",
                    (previous[0]["id"],),
                )
            ],
            "evidence": "previously accepted receipt; reuse is not a fresh verification",
        }
    source = value["source"]
    if not isinstance(source, dict) or set(source) != {
        "file_id",
        "item_id",
        "revision",
    }:
        raise CatabolicError("invalid receipt source")
    revision = source["revision"]
    for key in ("file_id", "item_id"):
        text(source[key], "source " + key, 255, empty=False)
    if (
        not isinstance(revision, dict)
        or set(revision) != set(REVISION_FIELDS)
        or any(type(n) is not int for n in revision.values())
    ):
        raise CatabolicError("receipt source requires an exact numeric revision")
    outputs = value["outputs"]
    if not isinstance(outputs, list) or not 1 <= len(outputs) <= 32:
        raise CatabolicError("receipt must deliver 1..32 outputs")
    adapter = Outputs(app)
    source_snapshot = occurrence(app.store, app.profile, source["file_id"])
    if any(
        source_snapshot[k] != revision[k] for k in REVISION_FIELDS if k != "ctime_ns"
    ):
        raise CatabolicError("receipt source revision is stale")
    source_snapshot["ctime_ns"] = revision["ctime_ns"]
    records, seen = [], set()
    deadline = time.monotonic() + 3600
    # All descriptors and their revision checks remain live through validation.
    # Their exit checks occur inside the transaction, so any late change rolls back.
    validated = []
    with ExitStack() as stack:
        stack.enter_context(validated_source(source_snapshot))
        for output in outputs:
            if (
                not isinstance(output, dict)
                or set(output) - {"location", "path", "definition_id", "size", "sha256"}
                or not {"location", "path", "definition_id", "size"} <= set(output)
            ):
                raise CatabolicError("invalid receipt output fields")
            path = relative_path(output["path"])
            for key in ("location", "definition_id"):
                text(output[key], "output " + key, 255, empty=False)
            if type(output["size"]) is not int or output["size"] <= 0:
                raise CatabolicError("receipt output size must be positive")
            if "sha256" in output and (
                not isinstance(output["sha256"], str)
                or not re.fullmatch("[0-9a-f]{64}", output["sha256"])
            ):
                raise CatabolicError("invalid receipt output SHA-256")
            if app.store.rows(
                "SELECT 1 FROM generated_locations WHERE location=?",
                (output["location"],),
            ):
                raise CatabolicError(
                    "external workers cannot use Catabolic-owned generated locations"
                )
            files = app.store.rows(
                "SELECT id FROM files WHERE location=? AND path=?",
                (output["location"], path),
            )
            if (
                not files
                or files[0]["id"] == source["file_id"]
                or files[0]["id"] in seen
            ):
                raise CatabolicError(
                    "receipt output is unscanned, duplicated, or the source itself"
                )
            file_id = files[0]["id"]
            if app.store.rows(
                "SELECT 1 FROM media_outputs WHERE file_id=? AND profile=?",
                (file_id, app.profile),
            ):
                raise CatabolicError("receipt output is already registered")
            seen.add(file_id)
            definition = adapter.get_definition(output["definition_id"])["definition"]
            adapter.validate_source_item(
                source["file_id"], source["item_id"], definition
            )
            snapshot = occurrence(app.store, app.profile, file_id)
            if snapshot["size"] != output["size"]:
                raise CatabolicError("delivered output size differs from receipt")
            fd = stack.enter_context(validated_source(snapshot))
            snapshot["ctime_ns"] = os.fstat(fd).st_ctime_ns
            hasher = hashlib.sha256()
            while chunk := os.read(fd, 1024 * 1024):
                if time.monotonic() > deadline:
                    raise CatabolicError("receipt validation exceeded one hour")
                hasher.update(chunk)
            checksum = hasher.hexdigest()
            if output.get("sha256", checksum) != checksum:
                raise CatabolicError("delivered output checksum differs from receipt")
            validated.append((file_id, output["definition_id"], snapshot, checksum))
        with app.store.transaction() as db:
            for file_id, definition_id, snapshot, checksum in validated:
                result = adapter.record(
                    db,
                    file_id=file_id,
                    source_file_id=source["file_id"],
                    source_item_id=source["item_id"],
                    definition_id=definition_id,
                    metadata={
                        "producer": value["producer"],
                        "external_job_id": value["job_id"],
                    },
                )
                records.append((result["output_id"], encode(snapshot), checksum))
            identifier = str(uuid4())
            db.execute(
                "INSERT INTO external_receipts(id,profile,producer,instance,job_id,attempt,digest,payload) VALUES (?,?,?,?,?,?,?,?)",
                (identifier, *identity, digest, payload),
            )
            db.executemany(
                "INSERT INTO receipt_outputs VALUES (?,?,?,?)",
                [(identifier, *record) for record in records],
            )
            stack.close()
            if _accepted is not None:
                _accepted(db, identifier)
    return {
        "receipt_id": identifier,
        "reused": False,
        "output_ids": [r[0] for r in records],
        "evidence": "delivered size and SHA-256 verified; processing claims are producer-declared",
    }
