# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Read-only file processing with bounded subprocesses and durable CLI jobs."""

import errno
import hashlib
import json
import os
import re
import shutil
import threading
import time
from collections import OrderedDict, deque
from concurrent.futures import FIRST_COMPLETED, ThreadPoolExecutor, wait
from contextlib import contextmanager
from functools import lru_cache
from pathlib import Path
from uuid import uuid4

from .curation import bounded_rows, occurrence, page_limit, payload_object
from .domain import CatabolicError
from .process_runner import CommandFailure as ProcessingFailure
from .process_runner import command_output as command_output
from .source_access import validated_source as validated_source
from .store import encode

OPERATIONS = ("sniff", "hash", "verify", "probe", "text", "decode")
TERMINAL = (
    "complete",
    "partial",
    "unsupported",
    "timeout",
    "changed",
    "failed",
    "mismatch",
    "cancelled",
)


@contextmanager
def worker_pool(workers, cancel):
    pool = ThreadPoolExecutor(max_workers=workers)
    try:
        yield pool
    except BaseException:
        cancel.set()
        raise
    finally:
        pool.shutdown(wait=True, cancel_futures=True)


@lru_cache(maxsize=16)
def tool_version(path, size, modified):
    try:
        return (
            command_output([path, "-version"], timeout=5, maximum=65536)
            .decode("utf-8", "replace")
            .splitlines()[0]
        )
    except (OSError, ProcessingFailure, IndexError):
        return "version unavailable"


def tool_signature(operation):
    executable = (
        "ffprobe"
        if operation == "probe"
        else "ffmpeg"
        if operation == "decode"
        else None
    )
    if not executable:
        return {"adapter": operation + "-v1"}
    path = shutil.which(executable)
    if not path:
        return {"adapter": operation + "-v1", "tool": executable, "available": False}
    path = str(Path(path).resolve())
    s = os.stat(path)
    return {
        "adapter": operation + "-v1",
        "tool": path,
        "available": True,
        "version": tool_version(path, s.st_size, s.st_mtime_ns),
        "size": s.st_size,
        "mtime_ns": s.st_mtime_ns,
    }


def current_fact(store, profile, file_id, operation="probe"):
    rows = store.rows(
        "SELECT * FROM file_facts WHERE profile=? AND file_id=? AND operation=?",
        (profile, file_id, operation),
    )
    if not rows:
        return None
    row = rows[0]
    snapshot = json.loads(row["snapshot"])
    now = occurrence(store, profile, file_id)
    row["current"] = row["status"] == "complete" and all(
        now.get(k) == v for k, v in snapshot.items() if k != "ctime_ns"
    )
    row["data"] = json.loads(row["data"])
    row["snapshot"] = snapshot
    return row


def words(text):
    return set(re.findall(r"[^\W_]+", text.casefold(), flags=re.UNICODE))


def text_payload(raw, extension):
    try:
        text = raw.decode("utf-8-sig")
    except UnicodeDecodeError as exc:
        raise ProcessingFailure("unsupported", "text requires UTF-8 encoding") from exc
    segments = []
    if extension in (".srt", ".vtt"):
        for block in re.split(r"\r?\n\s*\r?\n", text):
            lines = block.splitlines()
            position = next((i for i, line in enumerate(lines) if "-->" in line), None)
            if position is None:
                continue
            timestamp = lines[position].split("-->", 1)[0].strip().replace(",", ".")
            try:
                parts = [float(p) for p in timestamp.split(":")]
                seconds = sum(n * 60**i for i, n in enumerate(reversed(parts)))
            except ValueError:
                continue
            segments.append(
                {
                    "locator": {"seconds": seconds},
                    "text": "\n".join(lines[position + 1 :]),
                }
            )
    else:
        segments = [
            {"locator": {"line": i + 1}, "text": line}
            for i, line in enumerate(text.splitlines())
            if line.strip()
        ]
    if len(segments) > 10000:
        raise ProcessingFailure("partial", "text exceeds 10000 segments")
    return {"segments": segments}


def process(job, cancel):
    snapshot = job["snapshot"]
    options = job["options"]
    operation = job["operation"]
    started = time.monotonic()
    try:
        with validated_source(snapshot) as fd:
            if operation == "sniff":
                raw = os.read(fd, min(options["max_bytes"], 4096))
                signatures = (
                    (b"\x89PNG\r\n\x1a\n", "image/png"),
                    (b"\xff\xd8\xff", "image/jpeg"),
                    (b"GIF8", "image/gif"),
                    (b"fLaC", "audio/flac"),
                    (b"OggS", "application/ogg"),
                    (b"ID3", "audio/mpeg"),
                    (b"\x1a\x45\xdf\xa3", "video/x-matroska"),
                    (b"%PDF-", "application/pdf"),
                    (b"PK\x03\x04", "application/zip"),
                )
                mime = next(
                    (
                        value
                        for signature, value in signatures
                        if raw.startswith(signature)
                    ),
                    None,
                )
                if raw[:4] == b"RIFF" and raw[8:12] == b"WAVE":
                    mime = "audio/wav"
                if raw[:4] == b"RIFF" and raw[8:12] == b"WEBP":
                    mime = "image/webp"
                if raw[4:8] == b"ftyp":
                    mime = "video/mp4"
                data = {
                    "mime": mime,
                    "extension": Path(snapshot["path"]).suffix.lower(),
                    "bytes_read": len(raw),
                    "coverage": "signature only",
                }
            elif operation in ("hash", "verify"):
                digest = hashlib.sha256()
                read = 0
                while True:
                    if cancel.is_set():
                        raise ProcessingFailure("cancelled", "processing cancelled")
                    if time.monotonic() - started > options["timeout"]:
                        raise ProcessingFailure(
                            "timeout", "hash exceeded elapsed-time limit"
                        )
                    chunk = os.read(fd, 1024 * 1024)
                    if not chunk:
                        break
                    digest.update(chunk)
                    read += len(chunk)
                data = {
                    "algorithm": "sha256",
                    "digest": digest.hexdigest(),
                    "size": read,
                    "bytes_read": read,
                }
            elif operation == "text":
                if Path(snapshot["path"]).suffix.lower() not in (
                    ".txt",
                    ".md",
                    ".srt",
                    ".vtt",
                ):
                    raise ProcessingFailure(
                        "unsupported", "text supports UTF-8 txt, md, srt and vtt files"
                    )
                with os.fdopen(os.dup(fd), "rb") as stream:
                    raw = stream.read(options["max_bytes"] + 1)
                if len(raw) > options["max_bytes"]:
                    raise ProcessingFailure("partial", "text exceeds the byte limit")
                data = text_payload(raw, Path(snapshot["path"]).suffix.lower())
            else:
                tool = options["extractor"]
                if tool != tool_signature(operation):
                    raise ProcessingFailure(
                        "changed", "extractor changed since enqueue; enqueue a new job"
                    )
                if not tool.get("available"):
                    raise ProcessingFailure(
                        "unsupported", f"optional tool {tool['tool']} is unavailable"
                    )
                path = f"/dev/fd/{fd}"
                if operation == "probe":
                    argv = [
                        tool["tool"],
                        "-v",
                        "error",
                        "-protocol_whitelist",
                        "file,pipe",
                        "-format_whitelist",
                        "mov,matroska,webm,wav,flac,mp3,ogg,aac,ac3,eac3,aiff,ape,asf,avi,mpeg,mpegts,png_pipe,jpeg_pipe,webp_pipe,gif,bmp_pipe,tiff_pipe",
                        "-max_alloc",
                        "33554432",
                        "-threads",
                        "1",
                        "-probesize",
                        str(options["analysis_bytes"]),
                        "-analyzeduration",
                        str(options["analysis_us"]),
                        "-show_format",
                        "-show_streams",
                        "-show_chapters",
                        "-of",
                        "json",
                        path,
                    ]
                    data = json.loads(
                        command_output(
                            argv,
                            pass_fds=(fd,),
                            timeout=options["timeout"],
                            maximum=options["max_bytes"],
                            cancel=cancel,
                        )
                    )
                    streams = data.get("streams", [])
                    if not isinstance(streams, list) or len(streams) > 256:
                        raise ProcessingFailure("partial", "probe exceeds 256 streams")
                    video = [
                        s
                        for s in streams
                        if s.get("codec_type") == "video"
                        and not s.get("disposition", {}).get("attached_pic")
                    ]
                    audio = [s for s in streams if s.get("codec_type") == "audio"]
                    fmt = data.get("format", {})
                    fmt.pop("filename", None)
                    data["summary"] = {
                        "width": max((s.get("width", 0) for s in video), default=None),
                        "height": max(
                            (s.get("height", 0) for s in video), default=None
                        ),
                        "video_codecs": sorted(
                            {s["codec_name"] for s in video if s.get("codec_name")}
                        ),
                        "audio_codecs": sorted(
                            {s["codec_name"] for s in audio if s.get("codec_name")}
                        ),
                        "languages": sorted(
                            {
                                s.get("tags", {}).get("language")
                                for s in streams
                                if s.get("tags", {}).get("language")
                            }
                        ),
                        "duration": fmt.get("duration"),
                        "format": fmt.get("format_name"),
                        "hdr": any(
                            s.get("color_transfer") in ("smpte2084", "arib-std-b67")
                            for s in video
                        )
                        if video and all(s.get("color_transfer") for s in video)
                        else None,
                    }
                    data["analysis"] = {
                        "bytes": options["analysis_bytes"],
                        "microseconds": options["analysis_us"],
                        "coverage": "bounded stream analysis; not a full decode",
                    }
                else:
                    command_output(
                        [
                            tool["tool"],
                            "-v",
                            "error",
                            "-xerror",
                            "-nostdin",
                            "-protocol_whitelist",
                            "file,pipe",
                            "-format_whitelist",
                            "mov,matroska,webm,wav,flac,mp3,ogg,aac,ac3,eac3,aiff,ape,asf,avi,mpeg,mpegts,png_pipe,jpeg_pipe,webp_pipe,gif,bmp_pipe,tiff_pipe",
                            "-max_alloc",
                            "33554432",
                            "-i",
                            path,
                            "-map",
                            "0:v?",
                            "-map",
                            "0:a?",
                            "-threads",
                            "1",
                            "-f",
                            "null",
                            "-",
                        ],
                        pass_fds=(fd,),
                        timeout=options["timeout"],
                        maximum=options["max_bytes"],
                        cancel=cancel,
                    )
                    data = {"decoded": True, "coverage": "complete audio/video decode"}
        encode(data)  # Reject nonfinite or otherwise unrepresentable extractor data.
        return {
            "state": "complete",
            "data": data,
            "error": None,
            "elapsed_seconds": time.monotonic() - started,
        }
    except ProcessingFailure as exc:
        return {
            "state": exc.state,
            "data": {},
            "error": str(exc),
            "retryable": exc.state == "timeout",
        }
    except CatabolicError as exc:
        return {"state": "changed", "data": {}, "error": str(exc)}
    except OSError as exc:
        transient = {
            errno.EIO,
            errno.EAGAIN,
            errno.EBUSY,
            errno.ETIMEDOUT,
            errno.ENETDOWN,
            errno.ENETUNREACH,
            errno.ECONNRESET,
            errno.EHOSTUNREACH,
            getattr(errno, "ESTALE", -1),
        }
        return {
            "state": "failed",
            "data": {},
            "error": str(exc)[:1000],
            "retryable": exc.errno in transient,
        }
    except (ValueError, TypeError, KeyError, RecursionError) as exc:
        return {"state": "failed", "data": {}, "error": str(exc)[:1000]}


class Processing:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def enqueue(
        self,
        operation,
        *,
        file_ids=None,
        location=None,
        limit=100,
        after="",
        refresh=False,
        options=None,
    ):
        page_limit(limit)
        if location is not None and not self.store.rows(
            "SELECT id FROM locations WHERE id=?", (location,)
        ):
            raise CatabolicError("unknown source location")
        if operation not in OPERATIONS:
            raise CatabolicError("unsupported processing operation")
        options = payload_object(options or {})
        if set(options) - {"timeout", "max_bytes", "analysis_bytes", "analysis_us"}:
            raise CatabolicError("unknown processing option")
        config = {
            "timeout": 120 if operation in ("hash", "verify", "decode") else 20,
            "max_bytes": 2 * 1024 * 1024,
            "analysis_bytes": 1024 * 1024,
            "analysis_us": 1000000,
            **options,
        }
        for key, maximum in (
            ("timeout", 86400),
            ("max_bytes", 8 * 1024 * 1024),
            ("analysis_bytes", 64 * 1024 * 1024),
            ("analysis_us", 60000000),
        ):
            if type(config[key]) is not int or not 1 <= config[key] <= maximum:
                raise CatabolicError(f"invalid {key} budget")
        config["extractor"] = tool_signature(operation)
        if file_ids is not None:
            if (
                not isinstance(file_ids, list)
                or len(file_ids) > 1000
                or any(not isinstance(v, str) for v in file_ids)
            ):
                raise CatabolicError("file_ids must contain at most 1000 IDs")
            ids = sorted(set(file_ids))
            next_after = None
        else:
            rows = self.store.rows(
                """SELECT f.id FROM files f JOIN observations o ON o.file_id=f.id AND o.profile=?
                WHERE o.status='present' AND f.id>? AND (? IS NULL OR f.location=?) ORDER BY f.id LIMIT ?""",
                (self.profile, after or "", location, location, limit + 1),
            )
            next_after = rows[limit - 1]["id"] if len(rows) > limit else None
            ids = [r["id"] for r in rows[:limit]]
        queued = []
        cached = []
        errors = []
        for file_id in ids:
            snapshot = occurrence(self.store, self.profile, file_id)
            try:
                with validated_source(snapshot) as fd:
                    snapshot["ctime_ns"] = os.fstat(fd).st_ctime_ns
            except (CatabolicError, OSError) as exc:
                errors.append({"file_id": file_id, "error": str(exc)})
                continue
            key = hashlib.sha256(
                encode([snapshot, operation, config]).encode()
            ).hexdigest()
            old = self.store.rows(
                """SELECT id,state FROM processing_jobs WHERE profile=? AND file_id=? AND operation=?
                AND cache_key=? AND state NOT IN ('cancelled') ORDER BY created_at DESC,id DESC LIMIT 1""",
                (self.profile, file_id, operation, key),
            )
            if (
                old
                and (not refresh or old[0]["state"] in ("queued", "running"))
                and operation != "verify"
            ):
                cached.append(old[0]["id"])
                continue
            if operation == "verify" and not self.store.rows(
                "SELECT 1 FROM content_baselines WHERE profile=? AND file_id=?",
                (self.profile, file_id),
            ):
                errors.append(
                    {
                        "file_id": file_id,
                        "error": "no checksum baseline; run hash first",
                    }
                )
                continue
            identifier = str(uuid4())
            with self.store.transaction() as db:
                db.execute(
                    "INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key) VALUES (?,?,?,?,?,?,?,?)",
                    (
                        identifier,
                        self.profile,
                        file_id,
                        operation,
                        snapshot["location"],
                        encode(snapshot),
                        encode(config),
                        key,
                    ),
                )
            queued.append(identifier)
        return {
            "queued": queued,
            "cached": cached,
            "errors": errors,
            "complete": not errors,
            "next_after": next_after,
        }

    def list(self, *, state=None, limit=100, after="", file_id=None):
        page_limit(limit)
        rows, more = bounded_rows(
            self.store,
            """SELECT id,profile,file_id,operation,state,attempts,error,created_at,finished_at,
            snapshot,options,CASE WHEN length(result)<=8192 THEN result END AS result,
            coalesce(length(result)>8192,0) AS result_omitted
            FROM processing_jobs WHERE profile=? AND id>? AND (? IS NULL OR state=?)
            AND (? IS NULL OR file_id=?) ORDER BY id LIMIT ?""",
            (self.profile, after or "", state, state, file_id, file_id, limit + 1),
            limit,
        )
        for row in rows:
            for key in ("snapshot", "options", "result"):
                row[key] = json.loads(row[key]) if row[key] else None
        return {"jobs": rows, "next_after": rows[-1]["id"] if more else None}

    def get(self, identifier):
        rows = self.store.rows(
            "SELECT * FROM processing_jobs WHERE id=? AND profile=?",
            (identifier, self.profile),
        )
        if not rows:
            raise CatabolicError("unknown job")
        row = rows[0]
        for key in ("snapshot", "options", "result"):
            row[key] = json.loads(row[key]) if row[key] is not None else None
        return row

    def attempts(self, identifier, *, limit=100, after=0):
        self.get(identifier)
        page_limit(limit)
        rows = self.store.rows(
            "SELECT * FROM processing_attempts WHERE job_id=? AND attempt>? ORDER BY attempt LIMIT ?",
            (identifier, after, limit + 1),
        )
        return {
            "attempts": rows[:limit],
            "next_after": rows[limit - 1]["attempt"] if len(rows) > limit else None,
        }

    def cancel(self, identifier):
        with self.store.transaction() as db:
            changed = db.execute(
                "UPDATE processing_jobs SET state='cancelled',finished_at=CURRENT_TIMESTAMP WHERE id=? AND profile=? AND state IN ('queued','running')",
                (identifier, self.profile),
            ).rowcount
        return {"cancelled": bool(changed), "id": identifier}

    def retry(self, identifier):
        with self.store.transaction() as db:
            changed = db.execute(
                "UPDATE processing_jobs SET state='queued',error=NULL,finished_at=NULL WHERE id=? AND profile=? AND state IN ('failed','timeout','cancelled')",
                (identifier, self.profile),
            ).rowcount
        if not changed:
            raise CatabolicError(
                "retry requires a failed, timed-out or cancelled job; changed files need a new job"
            )
        return {"queued": identifier}

    def _publish(self, job, result, *, retry_delay=30):
        state = result["state"]
        data = result["data"]
        error = result["error"]
        if state == "complete":
            now = occurrence(self.store, self.profile, job["file_id"])
            if any(
                now.get(k) != v for k, v in job["snapshot"].items() if k != "ctime_ns"
            ):
                state, error = "changed", "inventory changed before publishing"
            else:
                try:
                    with validated_source(job["snapshot"]):
                        pass
                except (CatabolicError, OSError) as exc:
                    state, error = "changed", str(exc)
        with self.store.transaction() as db:
            if state == "complete" and job["operation"] in ("hash", "verify"):
                baseline = db.execute(
                    "SELECT * FROM content_baselines WHERE profile=? AND file_id=? AND algorithm=?",
                    (self.profile, job["file_id"], "sha256"),
                ).fetchone()
                if baseline and baseline["digest"] != data["digest"]:
                    state, error = (
                        "mismatch",
                        "checksum differs from preserved baseline",
                    )
                    data["expected_digest"] = baseline["digest"]
                    db.execute(
                        "UPDATE file_facts SET status='invalidated' WHERE profile=? AND file_id=?",
                        (self.profile, job["file_id"]),
                    )
                elif not baseline:
                    db.execute(
                        "INSERT INTO content_baselines VALUES (?,?,?,?,?,?)",
                        (
                            self.profile,
                            job["file_id"],
                            "sha256",
                            data["digest"],
                            data["size"],
                            job["id"],
                        ),
                    )
            if state == "complete":
                db.execute(
                    """INSERT INTO file_facts VALUES (?,?,?,?,?,?,?) ON CONFLICT(profile,file_id,operation)
                    DO UPDATE SET job_id=excluded.job_id,snapshot=excluded.snapshot,data=excluded.data,status=excluded.status""",
                    (
                        self.profile,
                        job["file_id"],
                        job["operation"],
                        job["id"],
                        encode(job["snapshot"]),
                        encode(data),
                        state,
                    ),
                )
                if job["operation"] == "text":
                    db.execute(
                        "DELETE FROM text_segments WHERE profile=? AND file_id=?",
                        (self.profile, job["file_id"]),
                    )
                    for i, segment in enumerate(data["segments"]):
                        cursor = db.execute(
                            "INSERT INTO text_segments(profile,file_id,job_id,ordinal,locator,text) VALUES (?,?,?,?,?,?)",
                            (
                                self.profile,
                                job["file_id"],
                                job["id"],
                                i,
                                encode(segment["locator"]),
                                segment["text"],
                            ),
                        )
                        db.executemany(
                            "INSERT INTO text_terms VALUES (?,?)",
                            [
                                (term, cursor.lastrowid)
                                for term in words(segment["text"])
                            ],
                        )
            db.execute(
                "UPDATE processing_jobs SET state=?,result=?,error=?,finished_at=CURRENT_TIMESTAMP WHERE id=?",
                (state, encode(data), error, job["id"]),
            )
            attempt = db.execute(
                "SELECT attempts FROM processing_jobs WHERE id=?", (job["id"],)
            ).fetchone()[0]
            retryable = state in ("failed", "timeout") and result.get(
                "retryable", False
            )
            retry_after = time.time() + min(
                3600, retry_delay * 2 ** min(attempt - 1, 12)
            )
            db.execute(
                "DELETE FROM processing_retry_queue WHERE job_id=?", (job["id"],)
            )
            if retryable and attempt <= 10:
                db.execute(
                    "INSERT INTO processing_retry_queue VALUES (?,?,?,?)",
                    (job["id"], self.profile, attempt, retry_after),
                )
            db.execute(
                "INSERT INTO processing_attempts(job_id,attempt,state,error,retryable,retry_after) VALUES (?,?,?,?,?,?)",
                (
                    job["id"],
                    attempt,
                    state,
                    error,
                    int(retryable),
                    retry_after,
                ),
            )
        return state

    def run(
        self,
        *,
        workers=2,
        per_device=1,
        limit=100,
        storage_groups=None,
        retry_transient=0,
        retry_delay=30,
    ):
        if type(retry_transient) is not int or not 0 <= retry_transient <= 10:
            raise CatabolicError("retry transient must be 0..10 additional attempts")
        if type(retry_delay) is not int or not 1 <= retry_delay <= 3600:
            raise CatabolicError("retry delay must be 1..3600 seconds")
        if (
            type(workers) is not int
            or not 1 <= workers <= 16
            or type(per_device) is not int
            or not 1 <= per_device <= workers
        ):
            raise CatabolicError("workers must be 1..16; per_device must be 1..workers")
        if type(limit) is not int or not 1 <= limit <= 1000000:
            raise CatabolicError("run limit must be 1..1000000")
        storage_groups = payload_object(storage_groups or {})
        if any(
            not isinstance(v, str) or not v or len(v) > 128
            for v in storage_groups.values()
        ):
            raise CatabolicError(
                "storage groups must map source names to nonempty group names"
            )
        # Store's process-wide writer lock ensures no other runner owns these claims.
        with self.store.transaction() as db:
            # Claim due retries once per invocation; never sleep with the writer lock.
            due = db.execute(
                """SELECT j.id FROM processing_retry_queue a JOIN processing_jobs j ON j.id=a.job_id
                WHERE a.retry_after<=? AND a.profile=?
                AND j.attempts=a.attempt AND j.attempts<=? AND j.state IN ('failed','timeout')
                ORDER BY a.retry_after,a.job_id LIMIT ?""",
                (time.time(), self.profile, retry_transient, min(limit, 1000)),
            ).fetchall()
            db.executemany(
                "DELETE FROM processing_retry_queue WHERE job_id=?",
                [(r[0],) for r in due],
            )
            db.executemany(
                "UPDATE processing_jobs SET state='queued',finished_at=NULL WHERE id=?",
                [(r[0],) for r in due],
            )
            db.execute(
                "UPDATE processing_jobs SET state='queued' WHERE profile=? AND state='running'",
                (self.profile,),
            )
        bindings = self.store.rows(
            "SELECT owner,device FROM bindings WHERE profile=? AND kind='source' ORDER BY owner LIMIT 10001",
            (self.profile,),
        )
        if len(bindings) > 10000:
            raise CatabolicError("runner supports at most 10000 source bindings")
        queues = deque(
            [
                (
                    b["owner"],
                    (("device", b["device"]),)
                    + (
                        (("group", storage_groups[b["owner"]]),)
                        if b["owner"] in storage_groups
                        else ()
                    ),
                )
                for b in bindings
            ]
        )
        cancel = threading.Event()
        pending = {}
        devices = {}
        counts = {}
        submitted = 0
        reused = 0
        cache = OrderedDict()

        def physical_key(job):
            snapshot = job["snapshot"]
            return encode(
                [
                    job["operation"],
                    job["options"],
                    [
                        snapshot[k]
                        for k in ("device", "inode", "size", "mtime_ns", "ctime_ns")
                    ],
                    Path(snapshot["path"]).suffix.lower()
                    if job["operation"] == "text"
                    else None,
                ]
            )

        try:
            with worker_pool(workers, cancel) as pool:
                while pending or submitted < limit:
                    available = workers - len(pending)
                    if available and submitted < limit:
                        for source, device in queues:
                            slots = min(
                                per_device - devices.get(key, 0) for key in device
                            )
                            if slots <= 0:
                                continue
                            rows = self.store.rows(
                                "SELECT * FROM processing_jobs WHERE profile=? AND state='queued' AND location=? ORDER BY created_at,id LIMIT ?",
                                (
                                    self.profile,
                                    source,
                                    min(
                                        available,
                                        slots,
                                        limit - submitted,
                                    ),
                                ),
                            )
                            for row in rows:
                                job = dict(row)
                                job["snapshot"] = json.loads(job["snapshot"])
                                job["options"] = json.loads(job["options"])
                                with self.store.transaction() as db:
                                    db.execute(
                                        "UPDATE processing_jobs SET state='running',attempts=attempts+1 WHERE id=?",
                                        (job["id"],),
                                    )
                                key = physical_key(job)
                                if key in cache:
                                    future = pool.submit(
                                        lambda result: json.loads(encode(result)),
                                        cache[key],
                                    )
                                    reused += 1
                                else:
                                    future = pool.submit(process, job, cancel)
                                pending[future] = (job, device)
                                for bucket in device:
                                    devices[bucket] = devices.get(bucket, 0) + 1
                                submitted += 1
                                available -= 1
                                if available == 0 or submitted == limit:
                                    break
                            if available == 0 or submitted == limit:
                                break
                    queues.rotate(-1)
                    if not pending:
                        break
                    done, _ = wait(pending, timeout=0.1, return_when=FIRST_COMPLETED)
                    for future in done:
                        job, device = pending.pop(future)
                        for bucket in device:
                            devices[bucket] -= 1
                        result = future.result()
                        state = self._publish(job, result, retry_delay=retry_delay)
                        if state == "complete":
                            key = physical_key(job)
                            cache[key] = result
                            cache.move_to_end(key)
                            # Keep only small records, so reuse cannot retain hundreds of MiB.
                            if len(encode(result).encode()) > 65536:
                                del cache[key]
                            while len(cache) > 64:
                                cache.popitem(last=False)
                        counts[state] = counts.get(state, 0) + 1
        except BaseException:
            cancel.set()
            raise
        return {
            "processed": sum(counts.values()),
            "retried": len(due),
            "reused_physical_results": reused,
            "counts": counts,
            "complete": all(k == "complete" for k in counts),
            "remaining": self.store.db.execute(
                "SELECT count(*) FROM processing_jobs WHERE profile=? AND state='queued'",
                (self.profile,),
            ).fetchone()[0],
        }

    def facts(self, file_id):
        return {
            "file_id": file_id,
            "facts": [
                v
                for op in OPERATIONS
                if (v := current_fact(self.store, self.profile, file_id, op))
                is not None
            ],
        }

    def duplicates(self, *, limit=100, after=""):
        page_limit(limit)
        rows = self.store.rows(
            """SELECT digest,algorithm,size,count(*) AS occurrences FROM content_baselines
            WHERE profile=? AND digest>? GROUP BY digest,algorithm,size HAVING count(*)>1 ORDER BY digest LIMIT ?""",
            (self.profile, after or "", limit + 1),
        )
        more = len(rows) > limit
        rows = rows[:limit]
        return {
            "groups": rows,
            "next_after": rows[-1]["digest"] if more else None,
            "evidence": "recorded full SHA-256 baselines; verify before relying on current equality",
        }

    def search(self, text, *, limit=100, after=0):
        page_limit(limit)
        terms = sorted(words(text))
        if not terms or len(terms) > 20 or len(text.encode()) > 4096:
            raise CatabolicError("search requires 1..20 words and at most 4096 bytes")
        clauses = (
            " AND ".join(
                "EXISTS (SELECT 1 FROM text_terms t WHERE t.segment_id=s.id AND t.term=?)"
                for _ in terms[1:]
            )
            or "1"
        )
        rows = self.store.rows(
            f"""SELECT s.* FROM text_terms seed JOIN text_segments s ON s.id=seed.segment_id
            WHERE seed.term=? AND seed.segment_id>? AND s.profile=? AND {clauses}
            ORDER BY seed.segment_id LIMIT ?""",
            (terms[0], after, self.profile, *terms[1:], limit + 1),
        )
        more = len(rows) > limit
        rows = rows[:limit]
        for row in rows:
            row["locator"] = json.loads(row["locator"])
            fact = current_fact(self.store, self.profile, row["file_id"], "text")
            row["current"] = bool(fact and fact["current"])
            row["text"] = row["text"][:2000]
        return {
            "hits": rows,
            "next_after": rows[-1]["id"] if more else None,
            "match": "all normalized words",
        }
