# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Explicit catalog-scoped admission of evidenced renditions into normal layouts."""

import hashlib
import json
import os
from contextlib import nullcontext

from .curation import occurrence
from .domain import CatabolicError
from .outputs import PURPOSES, RENDITIONS_SQL
from .processing import current_fact
from .source_access import validated_source
from .store import encode


def ready(app, output, _ancestors=()):
    """Check current metadata/evidence without decoding or hashing an entire file."""
    if output["id"] in _ancestors or len(_ancestors) >= 64:
        raise CatabolicError("rendition lineage cycle or depth exceeds 64")
    parents = app.store.rows(
        f"SELECT * FROM ({RENDITIONS_SQL}) WHERE profile=? AND file_id=?",
        (app.profile, output["source_file_id"]),
    )
    for parent in parents:
        ready(app, parent, (*_ancestors, output["id"]))
    if output["artifact_id"]:
        row = app.store.rows(
            "SELECT a.*,j.snapshot AS input_snapshot FROM processing_artifacts a JOIN processing_jobs j ON j.id=a.job_id WHERE a.id=? AND j.state='complete' AND a.state='ready'",
            (output["artifact_id"],),
        )
        if not row:
            raise CatabolicError("artifact is not ready")
        row = row[0]
        snapshot = json.loads(row["publication_snapshot"] or "{}")
        digest = row["sha256"]
        with validated_source(json.loads(row["input_snapshot"])):
            pass
    else:
        rows = app.store.rows(
            "SELECT ro.snapshot,ro.sha256,e.payload FROM receipt_outputs ro JOIN external_receipts e ON e.id=ro.receipt_id WHERE output_id=?",
            (output["id"],),
        )
        if not rows:
            raise CatabolicError("external output has no validated receipt")
        snapshot, digest = json.loads(rows[0]["snapshot"]), rows[0]["sha256"]
        source = json.loads(rows[0]["payload"])["source"]
        current = occurrence(app.store, app.profile, source["file_id"])
        if any(
            current[k] != v for k, v in source["revision"].items() if k != "ctime_ns"
        ):
            raise CatabolicError("external rendition source revision is stale")
        current["ctime_ns"] = source["revision"]["ctime_ns"]
        with validated_source(current):
            pass
        expected = {
            "transcode": "video",
            "preview": "video",
            "thumbnail": "video",
            "audio": "audio",
            "subtitle": "subtitle",
        }.get(output["purpose"])
        if expected:
            fact = current_fact(app.store, app.profile, output["file_id"])
            if (
                not fact
                or not fact["current"]
                or not any(
                    s.get("codec_type") == expected
                    for s in fact["data"].get("streams", [])
                )
            ):
                raise CatabolicError(
                    "external rendition requires a current probe with its declared stream kind"
                )
    observed = occurrence(app.store, app.profile, output["file_id"])
    with validated_source(observed) as fd:
        # Inventory intentionally omits ctime; publication must compare the live
        # revision with its receipt/artifact evidence, including in-place edits.
        observed["ctime_ns"] = os.fstat(fd).st_ctime_ns
        matches = bool(snapshot) and all(
            observed.get(k) == snapshot.get(k)
            for k in (
                "size",
                "mtime_ns",
                "ctime_ns",
                "device",
                "inode",
                "root",
                "root_device",
                "root_inode",
            )
        )
        if not matches:
            verified = current_fact(app.store, app.profile, output["file_id"], "verify")
            if not (
                verified
                and verified["current"]
                and verified["snapshot"].get("ctime_ns") == observed["ctime_ns"]
                and verified["data"].get("digest") == digest
            ):
                raise CatabolicError("rendition evidence is stale; scan and verify it")


class Publication:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def get(self, catalog):
        if not self.store.rows("SELECT 1 FROM catalogs WHERE id=?", (catalog,)):
            raise CatabolicError("unknown catalog")
        rows = self.store.rows(
            "SELECT * FROM rendition_policies WHERE profile=? AND catalog=?",
            (self.profile, catalog),
        )
        return {
            "catalog": catalog,
            "profile": self.profile,
            "definition": json.loads(rows[0]["definition"]) if rows else None,
        }

    def put(self, catalog, value, *, _db=None):
        from .item_workflow import text

        self.app.require_recovered()
        self.get(catalog)
        if not isinstance(value, dict) or set(value) - {
            "version",
            "purpose",
            "definition_id",
            "rule_id",
            "mode",
            "include_originals",
        }:
            raise CatabolicError("invalid rendition publication policy")
        value = {"version": 1, "mode": "all", "include_originals": False, **value}
        if (
            type(value["version"]) is not int
            or value["version"] != 1
            or value["mode"] not in ("all", "preferred")
            or type(value["include_originals"]) is not bool
        ):
            raise CatabolicError(
                "invalid rendition policy version, mode or include_originals"
            )
        if not any(k in value for k in ("purpose", "definition_id", "rule_id")):
            raise CatabolicError("choose a purpose, output definition or rule revision")
        if "purpose" in value and value["purpose"] not in PURPOSES:
            raise CatabolicError("unknown rendition purpose")
        for key in ("definition_id", "rule_id"):
            if key in value:
                text(value[key], "publication " + key, 255, empty=False)
        if "definition_id" in value and not self.store.rows(
            "SELECT 1 FROM output_definitions WHERE id=?", (value["definition_id"],)
        ):
            raise CatabolicError("unknown output definition")
        if "rule_id" in value and not self.store.rows(
            "SELECT 1 FROM processing_rules WHERE id=? AND profile=?",
            (value["rule_id"], self.profile),
        ):
            raise CatabolicError("unknown rule revision in this profile")
        serialized = encode(value)
        with nullcontext(_db) if _db is not None else self.store.transaction() as db:
            db.execute(
                "INSERT INTO rendition_policies VALUES (?,?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET definition=excluded.definition,digest=excluded.digest",
                (
                    self.profile,
                    catalog,
                    serialized,
                    hashlib.sha256(serialized.encode()).hexdigest(),
                ),
            )
        return self.get(catalog)

    def decide(self, catalog, output_id, excluded, reason):
        from .item_workflow import text

        self.get(catalog)
        text(reason, "decision reason", 4096, empty=False)
        if not self.store.rows(
            "SELECT 1 FROM media_outputs WHERE id=? AND profile=?",
            (output_id, self.profile),
        ):
            raise CatabolicError("unknown rendition in this profile")
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO rendition_decisions VALUES (?,?,?,?,?) ON CONFLICT(profile,catalog,output_id) DO UPDATE SET excluded=excluded.excluded,reason=excluded.reason",
                (self.profile, catalog, output_id, int(excluded), reason),
            )
        return {
            "catalog": catalog,
            "output_id": output_id,
            "excluded": excluded,
            "reason": reason,
        }

    def candidates(self, catalog):
        policy = self.get(catalog)["definition"]
        report = {"configured": policy is not None, "admitted": 0, "excluded": []}
        if policy is None:
            return None, report
        if policy["mode"] == "preferred":
            from .copy_selection import CopySelection

            if CopySelection(self.app).get(catalog) is None:
                raise CatabolicError(
                    "preferred rendition mode requires a catalog copy policy"
                )
        clauses, values = ["m.profile=?"], [self.profile]
        for key in ("purpose", "definition_id"):
            if key in policy:
                clauses.append("m." + key + "=?")
                values.append(policy[key])
        if "rule_id" in policy:
            clauses.append(
                "EXISTS (SELECT 1 FROM rule_jobs rj JOIN processing_artifacts a ON a.job_id=rj.job_id WHERE rj.rule_id=? AND a.id=m.artifact_id)"
            )
            values.append(policy["rule_id"])
        outputs = self.store.rows(
            f"SELECT m.* FROM ({RENDITIONS_SQL}) m WHERE {' AND '.join(clauses)} ORDER BY m.id LIMIT 10001",
            tuple(values),
        )
        if len(outputs) > 10000:
            raise CatabolicError(
                "publication exceeds 10000 renditions; narrow the policy"
            )
        decisions = {
            r["output_id"]: r["reason"]
            for r in self.store.rows(
                "SELECT * FROM rendition_decisions WHERE profile=? AND catalog=? AND excluded=1",
                (self.profile, catalog),
            )
        }
        admitted = []
        for output in outputs:
            reason = decisions.get(output["id"])
            if reason is None:
                try:
                    ready(self.app, output)
                except (OSError, CatabolicError) as exc:
                    reason = str(exc)
            if reason is not None:
                report["excluded"].append({"output_id": output["id"], "reason": reason})
                continue
            admitted.extend(
                self.store.rows(
                    "SELECT a.*,f.path,f.location FROM item_files a JOIN files f ON f.id=a.file_id JOIN output_definitions d ON d.id=? WHERE a.file_id=? AND a.item_id=? AND a.role=json_extract(d.definition,'$.role')",
                    (output["definition_id"], output["file_id"], output["item_id"]),
                )
            )
        report.update(admitted=len(admitted), definition=policy)
        return admitted, report


def fallback_evidence(app, output, mode, ancestors=()):
    """Capture bounded evidence without opening files or changing legacy readiness.

    Live validation consumes the returned snapshots after the session closes.
    Accepted lineage never implies that any ancestor was checked live.
    """
    if output["id"] in ancestors or len(ancestors) >= 64:
        raise CatabolicError("rendition lineage cycle or depth limit")
    checks = []
    if output["artifact_id"]:
        rows = app.store.rows(
            "SELECT a.publication_snapshot,a.sha256,j.snapshot FROM processing_artifacts a JOIN processing_jobs j ON j.id=a.job_id WHERE a.id=? AND a.state='ready' AND j.state='complete'",
            (output["artifact_id"],),
        )
        if not rows:
            raise CatabolicError("rendition_not_ready")
        accepted = json.loads(rows[0]["publication_snapshot"] or "{}")
        source = json.loads(rows[0]["snapshot"])
        digest = rows[0]["sha256"]
    else:
        rows = app.store.rows(
            "SELECT r.snapshot,r.sha256,e.payload FROM receipt_outputs r JOIN external_receipts e ON e.id=r.receipt_id WHERE r.output_id=?",
            (output["id"],),
        )
        if not rows:
            raise CatabolicError("missing_output_evidence")
        accepted, digest = json.loads(rows[0]["snapshot"]), rows[0]["sha256"]
        receipt = json.loads(rows[0]["payload"])["source"]
        source = {
            **occurrence(app.store, app.profile, receipt["file_id"]),
            **receipt["revision"],
        }
        expected = {
            "transcode": "video",
            "remux": "video",
            "preview": "video",
            "thumbnail": "video",
            "audio": "audio",
            "subtitle": "subtitle",
        }.get(output["purpose"])
        fact = current_fact(app.store, app.profile, output["file_id"])
        if expected and (
            not fact
            or not fact["current"]
            or not any(
                s.get("codec_type") == expected for s in fact["data"].get("streams", [])
            )
        ):
            raise CatabolicError("output_probe_required")
    observed = occurrence(app.store, app.profile, output["file_id"])
    keys = ("size", "mtime_ns", "device", "inode", "root", "root_device", "root_inode")
    if not accepted or any(observed.get(k) != accepted.get(k) for k in keys):
        verified = current_fact(app.store, app.profile, output["file_id"], "verify")
        if (
            not verified
            or not verified["current"]
            or verified["data"].get("digest") != digest
        ):
            raise CatabolicError("output_verification_required")
        accepted = verified["snapshot"]
    if "ctime_ns" not in accepted:
        raise CatabolicError("output_revision_evidence_missing")
    checks.append({**observed, "ctime_ns": accepted["ctime_ns"]})
    current = occurrence(app.store, app.profile, output["source_file_id"])
    currentness = "not_live_checked"
    if any(current.get(k) != source.get(k) for k in keys):
        currentness = "known_changed"
    if mode == "current_source_revision":
        if currentness == "known_changed" or current.get("status") != "present":
            raise CatabolicError("source_currentness_unknown_or_changed")
        checks.append(
            {
                **current,
                **({"ctime_ns": source["ctime_ns"]} if "ctime_ns" in source else {}),
            }
        )
    parents = app.store.rows(
        f"SELECT * FROM ({RENDITIONS_SQL}) WHERE profile=? AND file_id=?",
        (app.profile, output["source_file_id"]),
    )
    for parent in parents if mode == "current_source_revision" else []:
        parent_checks, _ = fallback_evidence(
            app, parent, mode, (*ancestors, output["id"])
        )
        if mode == "current_source_revision":
            checks.extend(parent_checks)
    return checks, {
        "lineage_requirement": mode,
        "source_currentness": currentness,
        "accepted_source_revision": source,
        "output_digest": digest,
    }
