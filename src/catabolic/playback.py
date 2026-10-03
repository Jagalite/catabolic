# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Authorized, revision-pinned HLS sessions and bounded shared temporary encodes.

These files are a playback cache, never validated catalog renditions. Only closed
segments named by an atomically published playlist are exposed to clients.
"""

import hashlib
import json
import os
import stat
import time
from contextlib import contextmanager
from uuid import uuid4

from . import rendering
from .access import AccessError
from .app import Application
from .artifacts import Artifacts
from .content_access import authorize, revision_of, snapshot
from .domain import CatabolicError
from .playback_cache import (
    SEGMENT,
    destination_root,
    job_directory,
    playlist,
    reserved_bytes,
)
from .source_access import validated_source
from .store import encode

PRESETS = {"h264-720p", "h264-1080p"}


def permitted(access, body):
    if not access.processing(body):
        raise AccessError("not_found", 404)
    if not any(
        g.get("operator")
        or (g.get("derivatives") and body["item_id"] in access.members(g))
        for g in access.matching("content:read")
    ):
        raise AccessError("playback_content_not_authorized", 403)
    operation = access.store.rows(
        "SELECT * FROM api_operations WHERE id=? AND profile=? AND enabled=1",
        (body["operation_id"], access.profile),
    )
    if not operation:
        raise AccessError("operation_not_approved", 403)
    operation = operation[0]
    if not access.store.rows(
        "SELECT 1 FROM api_sources WHERE profile=? AND location=?",
        (access.profile, operation["location"]),
    ):
        raise AccessError("source_not_exposed", 403)
    if not access.store.rows(
        "SELECT 1 FROM item_files WHERE item_id=? AND file_id=? AND active=1 AND role IN ('primary','source')",
        (body["item_id"], body["source_file_id"]),
    ):
        raise AccessError("source_not_associated", 409)
    if (
        revision_of(access.store, access.profile, body["source_file_id"])
        != body["source_revision"]
    ):
        raise AccessError("revision_mismatch", 409)
    return operation


def existing_result(access, body, operation, source):
    artifacts = Artifacts(Application(access.store, access.profile))
    rows = access.store.rows(
        """SELECT a.id,o.file_id,o.id AS rendition_id,j.snapshot FROM processing_jobs j
        JOIN processing_artifacts a ON a.job_id=j.id JOIN media_outputs o ON o.artifact_id=a.id
        WHERE j.profile=? AND j.file_id=? AND j.recipe_id=? AND j.state='complete'
        AND a.state='ready' AND o.source_item_id=? AND a.location=? ORDER BY j.created_at DESC LIMIT 20""",
        (
            access.profile,
            body["source_file_id"],
            operation["recipe_id"],
            body["item_id"],
            operation["location"],
        ),
    )
    for row in rows:
        old = json.loads(row["snapshot"])
        if any(
            old.get(k) != source[k]
            for k in (
                "location",
                "path",
                "size",
                "mtime_ns",
                "device",
                "inode",
                "root",
                "root_device",
                "root_inode",
                "source_policy",
                "volume_uuid",
                "ctime_ns",
            )
        ):
            continue
        if not artifacts._usable(artifacts.get(row["id"]), full=False):
            continue
        revision = revision_of(access.store, access.profile, row["file_id"])
        try:
            authorize(access, row["file_id"], revision)
        except AccessError:
            continue
        return {
            "file_id": row["file_id"],
            "revision": revision,
            "content_path": f"/v1/files/{row['file_id']}/content?revision={revision}",
            "source_snapshot": source,
        }
    return None


def admit(access, body, key, ttl=3600):
    with access.store.transaction():
        return _admit(access, body, key, ttl)


def _admit(access, body, key, ttl):
    access.require("processing:request")
    if not isinstance(key, str) or not 1 <= len(key) <= 128:
        raise AccessError("invalid_idempotency_key", 400)
    if (
        set(body) != {"item_id", "source_file_id", "source_revision", "operation_id"}
        or any(not isinstance(v, str) or not 1 <= len(v) <= 256 for v in body.values())
        or type(ttl) is not int
        or not 60 <= ttl <= 21600
    ):
        raise AccessError("invalid_playback_request", 400)
    operation = permitted(access, body)
    stored_body = encode({**body, "ttl": ttl})
    rows = access.store.rows(
        "SELECT * FROM api_playback_sessions WHERE principal=? AND idempotency_key=?",
        (access.principal, key),
    )
    if rows:
        if rows[0]["body"] != stored_body:
            raise AccessError("idempotency_conflict", 409)
        report = status(access, rows[0]["id"])
        with access.store.transaction() as db:
            db.execute(
                "UPDATE api_playback_sessions SET token_id=? WHERE id=?",
                (access.token["id"], rows[0]["id"]),
            )
        return report
    store = access.store
    now = time.time()
    pending = store.rows(
        "SELECT principal,count(*) AS count FROM api_playback_sessions WHERE cancelled=0 AND expires>? GROUP BY principal",
        (now,),
    )
    if (
        sum(r["count"] for r in pending) >= 100
        or sum(r["count"] for r in pending if r["principal"] == access.principal) >= 10
    ):
        raise AccessError("playback_session_limit", 429)
    artifacts = Artifacts(Application(store, access.profile))
    recipe = artifacts.get_recipe(operation["recipe_id"])["definition"]
    if recipe["preset"] not in PRESETS:
        raise AccessError("playback_profile_unsupported", 409)
    source = snapshot(store, access.profile, body["source_file_id"])
    with validated_source(source) as fd:
        source["ctime_ns"] = os.fstat(fd).st_ctime_ns
    result = existing_result(access, body, operation, source)
    job_id = None
    if result is None:
        tools = rendering.tools()
        binding = Application(store, access.profile).binding(
            "source", operation["location"]
        )
        cache_key = hashlib.sha256(
            encode([body["item_id"], source, recipe, tools, binding, "hls-v1"]).encode()
        ).hexdigest()
        jobs = store.rows(
            "SELECT * FROM api_playback_jobs WHERE profile=? AND cache_key=? AND state IN ('queued','running','ready')",
            (access.profile, cache_key),
        )
        if jobs and jobs[0]["state"] == "ready":
            try:
                with job_directory(store, jobs[0]) as fd:
                    raw, segments, _ = playlist(fd)
                    if not segments or "#EXT-X-ENDLIST" not in raw:
                        raise AccessError("invalid_playback_cache", 409)
                    for segment in segments:
                        st = os.stat(segment, dir_fd=fd, follow_symlinks=False)
                        if not stat.S_ISREG(st.st_mode) or st.st_size <= 0:
                            raise AccessError("invalid_playback_cache", 409)
            except (OSError, CatabolicError):
                with store.transaction() as db:
                    db.execute(
                        "UPDATE api_playback_jobs SET state='stale',reserved_bytes=0,updated_at=? WHERE id=?",
                        (now, jobs[0]["id"]),
                    )
                jobs = []
        if not jobs or jobs[0]["state"] != "ready":
            workers = store.rows(
                "SELECT * FROM api_worker_status WHERE profile=?", (access.profile,)
            )
            if not workers or time.time() - workers[0]["updated_at"] > 30:
                raise AccessError("worker_unavailable", 409)
            try:
                os.kill(workers[0]["worker_pid"], 0)
            except OSError:
                raise AccessError("worker_unavailable", 409) from None
            caps = json.loads(workers[0]["capabilities"])
            if not caps.get("playback_hls") or caps["tools"] != tools:
                raise AccessError("playback_worker_unavailable", 409)
        if jobs:
            job_id = jobs[0]["id"]
        else:
            with destination_root(store, access.profile, binding) as root:
                fs, device = os.fstatvfs(root), os.fstat(root).st_dev
            if (
                fs.f_bavail * fs.f_frsize
                - reserved_bytes(store, device)
                - recipe["max_output_bytes"]
                < recipe["reserve_bytes"]
            ):
                raise AccessError("storage_reservation_unavailable", 409)
            job_id = str(uuid4())
            with store.transaction() as db:
                db.execute(
                    """INSERT INTO api_playback_jobs(id,profile,cache_key,body,snapshot,recipe,tools,destination,
                    reserved_bytes,reservation_device,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)""",
                    (
                        job_id,
                        access.profile,
                        cache_key,
                        encode(body),
                        encode(source),
                        encode(recipe),
                        encode(tools),
                        encode(binding),
                        recipe["max_output_bytes"],
                        device,
                        now,
                        now,
                    ),
                )
    identifier = str(uuid4())
    with store.transaction() as db:
        db.execute(
            "INSERT INTO api_playback_sessions(id,profile,principal,token_id,idempotency_key,body,job_id,result,created_at,expires) VALUES (?,?,?,?,?,?,?,?,?,?)",
            (
                identifier,
                access.profile,
                access.principal,
                access.token["id"],
                key,
                stored_body,
                job_id,
                encode(result) if result else None,
                now,
                now + ttl,
            ),
        )
    return status(access, identifier)


def session_row(access, identifier):
    rows = access.store.rows(
        "SELECT * FROM api_playback_sessions WHERE id=? AND profile=? AND principal=?",
        (identifier, access.profile, access.principal),
    )
    if not rows:
        raise AccessError("not_found", 404)
    row = rows[0]
    body = json.loads(row["body"])
    body.pop("ttl")
    permitted(access, body)
    with validated_source(
        snapshot(access.store, access.profile, body["source_file_id"])
    ):
        pass
    if row["expires"] <= time.time():
        raise AccessError("playback_session_expired", 410)
    return row, body


def live_job(access, row, body):
    if row["cancelled"]:
        raise AccessError("playback_session_cancelled", 410)
    if row["job_id"] is None:
        raise AccessError("not_found", 404)
    job = access.store.rows(
        "SELECT * FROM api_playback_jobs WHERE id=?", (row["job_id"],)
    )[0]
    if job["state"] not in ("running", "ready"):
        raise AccessError(
            "playback_not_ready" if job["state"] == "queued" else "playback_failed", 409
        )
    with validated_source(json.loads(job["snapshot"])):
        pass
    if json.loads(job["tools"]) != rendering.tools():
        raise AccessError("playback_tools_changed", 409)
    return job


def status(access, identifier):
    row, body = session_row(access, identifier)
    state, count, seconds, blockers = "ready", 0, 0.0, []
    content_path, playlist_path = None, None
    if row["cancelled"]:
        state = "cancelled"
    elif row["result"]:
        result = json.loads(row["result"])
        with validated_source(result["source_snapshot"]):
            pass
        authorize(access, result["file_id"], result["revision"])
        with validated_source(
            snapshot(access.store, access.profile, result["file_id"])
        ):
            pass
        content_path = result["content_path"]
    else:
        job = access.store.rows(
            "SELECT * FROM api_playback_jobs WHERE id=?", (row["job_id"],)
        )[0]
        state = job["state"]
        if state in ("running", "ready"):
            live_job(access, row, body)
            with job_directory(access.store, job) as directory:
                raw, segments, seconds = playlist(directory)
            if state == "ready" and (not segments or "#EXT-X-ENDLIST" not in raw):
                raise AccessError("invalid_playback_cache", 409)
            count = len(segments)
            if count:
                playlist_path = f"/v1/playback-sessions/{identifier}/index.m3u8"
                state = "playing" if state == "running" else "ready"
        if job["error"]:
            blockers = [job["error"]]
    return {
        "session_id": identifier,
        "state": state,
        "status_url": f"/v1/playback-sessions/{identifier}",
        "playlist_path": playlist_path,
        "content_path": content_path,
        "segment_count": count,
        "available_seconds": seconds,
        "expires": row["expires"],
        "blockers": blockers,
    }


def cancel(access, identifier):
    session_row(access, identifier)
    with access.store.transaction() as db:
        db.execute(
            "UPDATE api_playback_sessions SET cancelled=1 WHERE id=?", (identifier,)
        )
    return status(access, identifier)


@contextmanager
def media(access, identifier, segment=None):
    row, body = session_row(access, identifier)
    job = live_job(access, row, body)
    with job_directory(access.store, job) as directory:
        raw, segments, _ = playlist(directory)
        if not segments:
            raise AccessError("playback_not_ready", 409)
        if segment is None:
            lines = [
                f"segments/{line}/content" if SEGMENT.fullmatch(line) else line
                for line in raw.splitlines()
            ]
            yield ("\n".join(lines) + "\n").encode()
        else:
            if not SEGMENT.fullmatch(segment) or segment not in segments:
                raise AccessError("not_found", 404)
            fd = os.open(
                segment, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory
            )
            try:
                st = os.fstat(fd)
                if not stat.S_ISREG(st.st_mode) or st.st_size <= 0:
                    raise AccessError("invalid_playback_cache", 409)
                yield fd, st
            finally:
                os.close(fd)
