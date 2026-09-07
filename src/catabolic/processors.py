# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Durable external dispatch and fenced leases; receipt import owns registration."""

import hashlib
import json
import re
import secrets
import urllib.parse

from .curation import page_limit
from .domain import CatabolicError, name
from .item_workflow import text
from .network_adapters import request, token
from .outputs import Outputs
from .receipts import import_receipt, source_receipt
from .store import encode


def now(db):
    return db.execute("SELECT unixepoch('now')").fetchone()[0]


def seconds(value):
    if type(value) is not int or not 30 <= value <= 3600:
        raise CatabolicError("lease seconds must be 30..3600")
    return value


class Processors:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def get(self, identifier):
        rows = self.store.rows(
            "SELECT definition FROM processors WHERE profile=? AND id=?",
            (self.profile, identifier),
        )
        if not rows:
            raise CatabolicError("unknown processor in this profile")
        return {"id": identifier, **json.loads(rows[0]["definition"])}

    def put(self, identifier, definition):
        self.app.require_recovered()
        name(identifier)
        if not isinstance(definition, dict) or set(definition) - {
            "endpoint",
            "credential_env",
            "capacity",
            "timeout",
            "version",
        }:
            raise CatabolicError("invalid processor definition")
        value = {"version": 1, "capacity": 1, "timeout": 15, **definition}
        endpoint = value.get("endpoint")
        if not isinstance(endpoint, str) or len(endpoint) > 4096:
            raise CatabolicError("processor requires an HTTP(S) endpoint")
        parsed = urllib.parse.urlsplit(endpoint)
        if (
            parsed.scheme not in ("http", "https")
            or not parsed.hostname
            or parsed.username
            or parsed.password
            or parsed.query
            or parsed.fragment
        ):
            raise CatabolicError(
                "processor endpoint must be HTTP(S) without credentials, query or fragment"
            )
        if (
            type(value["version"]) is not int
            or value["version"] != 1
            or type(value["capacity"]) is not int
            or not 1 <= value["capacity"] <= 1000
            or type(value["timeout"]) is not int
            or not 1 <= value["timeout"] <= 60
        ):
            raise CatabolicError(
                "invalid processor version, capacity (1..1000) or timeout (1..60)"
            )
        if "credential_env" in value and (
            not isinstance(value["credential_env"], str)
            or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", value["credential_env"])
        ):
            raise CatabolicError("credential_env must name an environment variable")
        value["endpoint"] = endpoint.rstrip("/")
        serialized = encode(value)
        with self.store.transaction() as db:
            old = db.execute(
                "SELECT definition FROM processors WHERE profile=? AND id=?",
                (self.profile, identifier),
            ).fetchone()
            if old and old[0] != serialized:
                raise CatabolicError(
                    "processor definitions are immutable; choose a new name"
                )
            db.execute(
                "INSERT INTO processors VALUES (?,?,?) ON CONFLICT DO NOTHING",
                (identifier, self.profile, serialized),
            )
        return self.get(identifier)

    def enqueue(self, processor, file_id, item_id, definition_id, configuration):
        self.app.require_recovered()
        self.get(processor)
        definition = Outputs(self.app).get_definition(definition_id)
        if not isinstance(configuration, dict):
            raise CatabolicError("processor configuration must be an object")
        serialized = encode(configuration)
        if len(serialized.encode()) > 65536:
            raise CatabolicError("processor configuration exceeds 64 KiB")
        captured = source_receipt(self.app, file_id, item_id)
        location = self.store.rows(
            "SELECT location,path FROM files WHERE id=?", (file_id,)
        )[0]
        payload = {
            **captured,
            "version": 1,
            "source_location": location,
            "definition_id": definition_id,
            "output_definition": definition["definition"],
            "configuration": configuration,
            "configuration_digest": hashlib.sha256(serialized.encode()).hexdigest(),
        }
        serialized = encode(payload)
        digest = hashlib.sha256(serialized.encode()).hexdigest()
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO processor_jobs(id,profile,processor,request,digest) VALUES (?,?,?,?,?) ON CONFLICT(profile,processor,digest) DO NOTHING",
                (secrets.token_hex(16), self.profile, processor, serialized, digest),
            )
            identifier = db.execute(
                "SELECT id FROM processor_jobs WHERE profile=? AND processor=? AND digest=?",
                (self.profile, processor, digest),
            ).fetchone()[0]
        return self.job(identifier)

    def job(self, identifier, *, private=False):
        rows = self.store.rows(
            "SELECT * FROM processor_jobs WHERE profile=? AND id=?",
            (self.profile, identifier),
        )
        if not rows:
            raise CatabolicError("unknown processor job in this profile")
        row = rows[0]
        row["request"] = json.loads(row["request"])
        if not private:
            row.pop("lease_token")
        return row

    def jobs(self, processor, limit=100, after=""):
        self.get(processor)
        page_limit(limit)
        rows = self.store.rows(
            "SELECT id FROM processor_jobs WHERE profile=? AND processor=? AND id>? ORDER BY id LIMIT ?",
            (self.profile, processor, after, limit + 1),
        )
        return {
            "jobs": [self.job(row["id"]) for row in rows[:limit]],
            "next_after": rows[limit - 1]["id"] if len(rows) > limit else None,
        }

    def claim(self, processor, worker, lease_seconds=300, *, resume=False):
        self.app.require_recovered()
        config = self.get(processor)
        text(worker, "worker", 255, empty=False)
        seconds(lease_seconds)
        with self.store.transaction() as db:
            timestamp = now(db)
            row = None
            if resume:
                row = db.execute(
                    "SELECT * FROM processor_jobs WHERE profile=? AND processor=? AND worker=? AND state IN ('leased','submitted') AND lease_until>? ORDER BY updated_at,id LIMIT 1",
                    (self.profile, processor, worker, timestamp),
                ).fetchone()
            if row is None:
                active = db.execute(
                    "SELECT count(*) FROM processor_jobs WHERE profile=? AND processor=? AND state IN ('leased','submitted') AND lease_until>?",
                    (self.profile, processor, timestamp),
                ).fetchone()[0]
                if active >= config["capacity"]:
                    return {"claimed": False, "reason": "processor capacity is leased"}
                row = db.execute(
                    "SELECT * FROM processor_jobs WHERE profile=? AND processor=? AND (state='queued' OR (state IN ('leased','submitted') AND lease_until<=?)) ORDER BY created_at,id LIMIT 1",
                    (self.profile, processor, timestamp),
                ).fetchone()
                if row is None:
                    return {"claimed": False, "reason": "no queued or expired work"}
                db.execute(
                    "UPDATE processor_attempts SET state='expired',finished_at=? WHERE job_id=? AND generation=? AND finished_at IS NULL",
                    (timestamp, row["id"], row["generation"]),
                )
                db.execute(
                    "UPDATE processor_jobs SET state='leased',generation=generation+1,worker=?,lease_token=?,lease_until=?,error=NULL,updated_at=CURRENT_TIMESTAMP WHERE id=?",
                    (
                        worker,
                        secrets.token_hex(32),
                        timestamp + lease_seconds,
                        row["id"],
                    ),
                )
                db.execute(
                    "INSERT INTO processor_attempts VALUES (?,?,?,'leased',?,NULL)",
                    (row["id"], row["generation"] + 1, worker, timestamp),
                )
            else:
                db.execute(
                    "UPDATE processor_jobs SET lease_until=? WHERE id=?",
                    (timestamp + lease_seconds, row["id"]),
                )
            identifier = row["id"]
        return {"claimed": True, **self.job(identifier, private=True)}

    def _fence(self, db, identifier, lease_token):
        row = db.execute(
            "SELECT * FROM processor_jobs WHERE id=? AND profile=?",
            (identifier, self.profile),
        ).fetchone()
        if (
            row is None
            or not isinstance(lease_token, str)
            or not row["lease_token"]
            or not secrets.compare_digest(row["lease_token"], lease_token)
            or row["state"] not in ("leased", "submitted")
            or row["lease_until"] <= now(db)
        ):
            raise CatabolicError(
                "worker lease is expired, replaced or no longer active"
            )
        return row

    def heartbeat(self, identifier, lease_token, lease_seconds=300):
        seconds(lease_seconds)
        with self.store.transaction() as db:
            self._fence(db, identifier, lease_token)
            db.execute(
                "UPDATE processor_jobs SET lease_until=?,updated_at=CURRENT_TIMESTAMP WHERE id=?",
                (now(db) + lease_seconds, identifier),
            )
        return self.job(identifier)

    def transition(self, identifier, lease_token, state):
        if state not in ("submitted", "failed"):
            raise CatabolicError("invalid worker transition")
        with self.store.transaction() as db:
            row = self._fence(db, identifier, lease_token)
            db.execute(
                "UPDATE processor_jobs SET state=?,updated_at=CURRENT_TIMESTAMP WHERE id=?",
                (state, identifier),
            )
            db.execute(
                "UPDATE processor_attempts SET state=?,finished_at=? WHERE job_id=? AND generation=?",
                (
                    state,
                    now(db) if state == "failed" else None,
                    identifier,
                    row["generation"],
                ),
            )
        return self.job(identifier)

    def retry(self, identifier):
        row = self.job(identifier)
        if row["state"] not in ("failed", "cancelled"):
            raise CatabolicError(
                "only failed or cancelled jobs can be explicitly retried"
            )
        with self.store.transaction() as db:
            db.execute(
                "UPDATE processor_jobs SET state='queued',lease_token=NULL,lease_until=NULL,updated_at=CURRENT_TIMESTAMP WHERE id=?",
                (identifier,),
            )
        return self.job(identifier)

    def cancel(self, identifier):
        row = self.job(identifier)
        if row["state"] == "complete":
            raise CatabolicError("a completed receipt cannot be cancelled")
        with self.store.transaction() as db:
            db.execute(
                "UPDATE processor_jobs SET state='cancelled',lease_token=NULL,lease_until=NULL,updated_at=CURRENT_TIMESTAMP WHERE id=?",
                (identifier,),
            )
            db.execute(
                "UPDATE processor_attempts SET state='cancelled',finished_at=? WHERE job_id=? AND generation=? AND finished_at IS NULL",
                (now(db), identifier, row["generation"]),
            )
        return self.job(identifier)

    def complete(self, identifier, lease_token, receipt):
        row = self._fence(self.store.db, identifier, lease_token)
        payload = json.loads(row["request"])
        if not isinstance(receipt, dict) or any(
            receipt.get(key) != value
            for key, value in {
                "database_id": payload["database_id"],
                "profile": self.profile,
                "producer": row["processor"],
                "instance": row["worker"],
                "job_id": identifier,
                "attempt": str(row["generation"]),
                "source": payload["source"],
                "configuration_digest": payload["configuration_digest"],
            }.items()
        ):
            raise CatabolicError("processor receipt does not match the leased request")
        outputs = receipt.get("outputs")
        if (
            not isinstance(outputs, list)
            or not outputs
            or any(
                not isinstance(output, dict)
                or output.get("definition_id") != payload["definition_id"]
                for output in outputs
            )
        ):
            raise CatabolicError("processor output definition differs from the request")

        def accepted(db, receipt_id):
            current = self._fence(db, identifier, lease_token)
            db.execute(
                "UPDATE processor_jobs SET state='complete',receipt_id=?,lease_token=NULL,lease_until=NULL,updated_at=CURRENT_TIMESTAMP WHERE id=?",
                (receipt_id, identifier),
            )
            db.execute(
                "UPDATE processor_attempts SET state='complete',finished_at=? WHERE job_id=? AND generation=?",
                (now(db), identifier, current["generation"]),
            )

        return import_receipt(self.app, receipt, _accepted=accepted)


def dispatch_job(path, profile, identifier, lease_token):
    """Perform HTTP outside the writer lock, then recheck the lease on every write."""
    from .app import Application
    from .store import Store

    with Store(path) as store:
        processor = Processors(Application(store, profile))
        row = dict(processor._fence(store.db, identifier, lease_token))
        config = processor.get(row["processor"])
        payload = json.loads(row["request"])
        if row["state"] == "leased":
            captured = source_receipt(
                processor.app,
                payload["source"]["file_id"],
                payload["source"]["item_id"],
            )
            if captured["source"] != payload["source"]:
                raise CatabolicError(
                    "processor source changed since enqueue; enqueue a new revision"
                )
    headers = {"Content-Type": "application/json"}
    if config.get("credential_env"):
        headers["Authorization"] = "Bearer " + token(config["credential_env"])
    url = config["endpoint"] + "/jobs/" + identifier + "/" + str(row["generation"])
    # Repeating a PUT must return the same job. The processor must fence older
    # generations and keep their physical output paths distinct.
    body = encode(
        {
            **payload,
            "job_id": identifier,
            "attempt": str(row["generation"]),
            "producer": row["processor"],
            "instance": row["worker"],
        }
    ).encode()
    raw = request(
        url,
        method="PUT" if row["state"] == "leased" else "GET",
        body=body if row["state"] == "leased" else None,
        headers=headers,
        timeout=config["timeout"],
    )
    try:
        result = json.loads(raw)
    except (ValueError, RecursionError):
        raise CatabolicError("processor returned invalid JSON") from None
    if not isinstance(result, dict) or result.get("state") not in (
        "queued",
        "running",
        "complete",
        "failed",
    ):
        raise CatabolicError("processor returned an invalid job state")
    with Store(path, writable=True) as store:
        processor = Processors(Application(store, profile))
        if result["state"] == "complete":
            return processor.complete(identifier, lease_token, result.get("receipt"))
        return processor.transition(
            identifier,
            lease_token,
            "failed" if result["state"] == "failed" else "submitted",
        )
