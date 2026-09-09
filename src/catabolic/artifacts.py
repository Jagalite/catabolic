# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Recoverable generated files, with explicit ownership and source provenance."""

import hashlib
import json
import os
import stat
from pathlib import Path
from uuid import UUID, uuid4, uuid5

from . import rendering
from .curation import bounded_rows, occurrence, page_limit
from .domain import CatabolicError, name
from .filesystem import (
    claim_output,
    open_directory,
    owner_state,
    rename_noreplace,
    root_handle,
)
from .outputs import Outputs
from .process_runner import CommandFailure
from .source_access import validated_source
from .store import encode


class Artifacts:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def _owner(self, location):
        return {
            "database_id": self.store.database_id,
            "profile": self.profile,
            "location": location,
            "kind": "generated",
            "version": 1,
        }

    def bind(self, location, root):
        self.app.require_recovered()
        name(location)
        path = Path(root).resolve(strict=True)
        existing = self.store.rows(
            "SELECT * FROM generated_locations WHERE profile=? AND location=?",
            (self.profile, location),
        )
        if existing:
            binding = self.app.binding("source", location)
            if binding["root"] != str(path):
                raise CatabolicError(
                    "generated locations cannot be relocated in this version"
                )
            with root_handle(binding) as fd:
                if owner_state(fd, self._owner(location)) != "owned":
                    raise CatabolicError("generated location ownership is missing")
            return binding
        if self.store.rows("SELECT id FROM locations WHERE id=?", (location,)):
            raise CatabolicError(
                "use a new location; existing media locations cannot be adopted for writing"
            )
        self.app._validate_binding_path("source", location, path)
        fd = open_directory(path)
        try:
            claim_output(fd, self._owner(location), str(uuid4()))
            with self.store.transaction() as db:
                st = os.fstat(fd)
                db.execute("INSERT INTO locations VALUES (?)", (location,))
                db.execute(
                    "INSERT INTO bindings VALUES (?,?,?,?,?,?)",
                    (self.profile, "source", location, str(path), st.st_dev, st.st_ino),
                )
                db.execute(
                    "INSERT INTO generated_locations VALUES (?,?)",
                    (self.profile, location),
                )
        finally:
            os.close(fd)
        return self.app.binding("source", location)

    def recipe(self, recipe_name, preset, options=None, output_definition_id=None):
        name(recipe_name)
        value = rendering.definition(preset, options or {})
        outputs = Outputs(self.app)
        output = (
            outputs.get_definition(output_definition_id)
            if output_definition_id
            else self.default_output(preset)
        )
        value["output_definition_id"] = output["id"]
        serialized = encode(value)
        digest = hashlib.sha256(serialized.encode()).hexdigest()
        existing = self.store.rows(
            "SELECT * FROM processing_recipes WHERE name=? AND digest=?",
            (recipe_name, digest),
        )
        if existing:
            return self._decode(existing[0])
        identifier = str(uuid4())
        with self.store.transaction() as db:
            revision = db.execute(
                "SELECT coalesce(max(revision),0)+1 FROM processing_recipes WHERE name=?",
                (recipe_name,),
            ).fetchone()[0]
            db.execute(
                "INSERT INTO processing_recipes(id,name,revision,preset,definition,digest,output_definition_id) VALUES (?,?,?,?,?,?,?)",
                (
                    identifier,
                    recipe_name,
                    revision,
                    preset,
                    serialized,
                    digest,
                    output["id"],
                ),
            )
        return self.get_recipe(identifier)

    def default_output(self, preset):
        return Outputs(self.app).define(
            "builtin-" + preset, {"role": rendering.PRESETS[preset][2]}
        )

    def get_recipe(self, identifier):
        rows = self.store.rows(
            "SELECT * FROM processing_recipes WHERE id=?", (identifier,)
        )
        if not rows:
            raise CatabolicError(
                "unknown recipe ID; select an explicit immutable revision"
            )
        if rows[0].get("operation_kind", "render") != "render":
            raise CatabolicError("artifact execution requires a render operation")
        return self._decode(rows[0])

    @staticmethod
    def _decode(row):
        value = dict(row)
        for key in ("definition", "binding", "validation", "publication_snapshot"):
            if value.get(key) is not None:
                value[key] = json.loads(value[key])
        return value

    def recipes(self, limit=100, after=""):
        page_limit(limit)
        rows, more = bounded_rows(
            self.store,
            "SELECT * FROM processing_recipes WHERE id>? ORDER BY id LIMIT ?",
            (after, limit + 1),
            limit,
        )
        return {
            "recipes": [self._decode(r) for r in rows],
            "next_after": rows[-1]["id"] if more else None,
        }

    def list(self, *, limit=100, after="", state=None, file_id=None):
        page_limit(limit)
        rows, more = bounded_rows(
            self.store,
            """SELECT a.id,a.job_id,a.attempt,a.state,a.file_id,a.item_id,a.role,a.location,a.path,
               a.size,a.sha256,a.error,a.created_at,j.file_id AS input_file_id,j.recipe_id
               FROM processing_artifacts a JOIN processing_jobs j ON j.id=a.job_id
               WHERE a.profile=? AND a.id>? AND (? IS NULL OR a.state=?)
               AND (? IS NULL OR j.file_id=?) ORDER BY a.id LIMIT ?""",
            (self.profile, after, state, state, file_id, file_id, limit + 1),
            limit,
        )
        return {"artifacts": rows, "next_after": rows[-1]["id"] if more else None}

    def get(self, identifier):
        rows = self.store.rows(
            "SELECT * FROM processing_artifacts WHERE id=? AND profile=?",
            (identifier, self.profile),
        )
        if not rows:
            raise CatabolicError("unknown artifact")
        return self._decode(rows[0])

    def enqueue(self, file_id, recipe_id, location, item_id, *, _capabilities=None):
        self.app.require_recovered()
        recipe = self.get_recipe(recipe_id)
        output = (
            Outputs(self.app).get_definition(recipe["output_definition_id"])
            if recipe["output_definition_id"]
            else self.default_output(recipe["preset"])
        )
        Outputs(self.app).validate_source_item(file_id, item_id, output["definition"])
        if not self.store.rows(
            "SELECT 1 FROM generated_locations WHERE profile=? AND location=?",
            (self.profile, location),
        ):
            raise CatabolicError(
                "destination must be an explicitly bound generated location"
            )
        binding = self.app.binding("source", location)
        with root_handle(binding) as fd:
            if owner_state(fd, self._owner(location)) != "owned":
                raise CatabolicError("generated location ownership is missing")
        snapshot = occurrence(self.store, self.profile, file_id)
        with validated_source(snapshot) as fd:
            snapshot["ctime_ns"] = os.fstat(fd).st_ctime_ns
        available = (
            _capabilities if _capabilities is not None else rendering.capabilities()
        )
        capability = next(
            p for p in available["presets"] if p["name"] == recipe["preset"]
        )
        if not capability["available"]:
            missing = ", ".join(capability.get("missing", []))
            raise CatabolicError(
                "the installed FFmpeg build lacks required encoders, muxers or filters"
                + (": " + missing if missing else "")
            )
        options = {
            "recipe": recipe["definition"],
            "tools": available["tools"],
            "destination": binding,
            "item_id": item_id,
            "output_definition": output,
        }
        key = hashlib.sha256(
            encode([snapshot, recipe["digest"], options]).encode()
        ).hexdigest()
        rows = self.store.rows(
            "SELECT id,state FROM processing_jobs WHERE profile=? AND operation='render' AND cache_key=? ORDER BY created_at DESC,id DESC LIMIT 1",
            (self.profile, key),
        )
        if rows:
            old = rows[0]
            if old["state"] in ("queued", "running"):
                return {"job_id": old["id"], "reused": True}
            if old["state"] == "complete":
                artifacts = self.store.rows(
                    "SELECT id FROM processing_artifacts WHERE job_id=? AND state='ready'",
                    (old["id"],),
                )
                if artifacts and self._usable(self.get(artifacts[0]["id"])):
                    return {
                        "job_id": old["id"],
                        "artifact_id": artifacts[0]["id"],
                        "reused": True,
                    }
        identifier = str(uuid4())
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO processing_jobs(id,profile,file_id,operation,location,snapshot,options,cache_key,recipe_id) VALUES (?,?,?,?,?,?,?,?,?)",
                (
                    identifier,
                    self.profile,
                    file_id,
                    "render",
                    snapshot["location"],
                    encode(snapshot),
                    encode(options),
                    key,
                    recipe_id,
                ),
            )
        return {"job_id": identifier, "reused": False}

    def _check_fd(self, artifact, root, leaf):
        fd = os.open(leaf, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=root)
        try:
            st = os.fstat(fd)
            if not stat.S_ISREG(st.st_mode) or (st.st_dev, st.st_ino, st.st_size) != (
                artifact["device"],
                artifact["inode"],
                artifact["size"],
            ):
                raise CatabolicError(
                    "artifact identity or size changed; refusing publication"
                )
            if rendering.digest(fd) != artifact["sha256"]:
                raise CatabolicError("artifact checksum changed; refusing publication")
            after = os.fstat(fd)
            named = os.stat(leaf, dir_fd=root, follow_symlinks=False)
            keys = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns")
            if any(
                getattr(st, key) != getattr(after, key)
                or getattr(st, key) != getattr(named, key)
                for key in keys
            ):
                raise CatabolicError("artifact changed while verifying its checksum")
            return st
        finally:
            os.close(fd)

    def _usable(self, artifact):
        try:
            with root_handle(artifact["binding"]) as root:
                if owner_state(root, self._owner(artifact["location"])) != "owned":
                    return False
                self._check_fd(artifact, root, artifact["path"])
            return True
        except (OSError, CatabolicError):
            return False

    def _finish_attempt(self, db, job_id, attempt, state, result=None, error=None):
        db.execute(
            "UPDATE processing_attempts SET state=?,result=?,error=?,finished_at=CURRENT_TIMESTAMP WHERE job_id=? AND attempt=?",
            (
                state,
                encode(result) if result is not None else None,
                error,
                job_id,
                attempt,
            ),
        )
        db.execute(
            "UPDATE processing_jobs SET state=?,result=?,error=?,finished_at=CURRENT_TIMESTAMP WHERE id=?",
            (state, encode(result) if result is not None else None, error, job_id),
        )

    def _publish(self, artifact):
        binding = self.app.binding("source", artifact["location"])
        if binding != artifact["binding"]:
            raise CatabolicError("generated binding changed during publication")
        with root_handle(binding) as root:
            if owner_state(root, self._owner(artifact["location"])) != "owned":
                raise CatabolicError("generated ownership changed")
            try:
                os.stat(artifact["path"], dir_fd=root, follow_symlinks=False)
            except FileNotFoundError:
                self._check_fd(artifact, root, artifact["temporary_path"])
                rename_noreplace(
                    root, artifact["temporary_path"], root, artifact["path"]
                )
                os.fsync(root)
            st = self._check_fd(artifact, root, artifact["path"])
            with root_handle(binding):
                pass
            self._register(artifact, st)

    def _register(self, artifact, st):
        job = self.store.rows(
            "SELECT * FROM processing_jobs WHERE id=?", (artifact["job_id"],)
        )[0]
        options = json.loads(job["options"])
        output = options.get("output_definition") or self.default_output(
            options["recipe"]["preset"]
        )
        Outputs(self.app).validate_source_item(
            job["file_id"], options["item_id"], output["definition"]
        )
        identifier = str(
            uuid5(
                UUID(self.store.database_id),
                encode([artifact["location"], artifact["path"]]),
            )
        )
        scan = str(uuid4())
        binding = artifact["binding"]
        snapshot = {
            "id": identifier,
            "location": artifact["location"],
            "path": artifact["path"],
            "root": binding["root"],
            "root_device": binding["device"],
            "root_inode": binding["inode"],
            "size": st.st_size,
            "mtime_ns": st.st_mtime_ns,
            "ctime_ns": st.st_ctime_ns,
            "device": st.st_dev,
            "inode": st.st_ino,
            "status": "present",
        }
        with self.store.transaction() as db:
            # Recovery never adopts another scanner's or user's file record.
            db.execute(
                "INSERT INTO files(id,location,path) VALUES (?,?,?)",
                (identifier, artifact["location"], artifact["path"]),
            )
            db.execute(
                "INSERT INTO scans(id,profile,location,complete,observed,errors) VALUES (?,?,?,1,1,'[]')",
                (scan, self.profile, artifact["location"]),
            )
            db.execute(
                "INSERT INTO observations(profile,file_id,size,mtime_ns,device,inode,status,scan_id) VALUES (?,?,?,?,?,?,'present',?)",
                (
                    self.profile,
                    identifier,
                    st.st_size,
                    st.st_mtime_ns,
                    st.st_dev,
                    st.st_ino,
                    scan,
                ),
            )
            output_record = Outputs(self.app).record(
                db,
                file_id=identifier,
                source_file_id=job["file_id"],
                source_item_id=options["item_id"],
                definition_id=output["id"],
                artifact_id=artifact["id"],
                metadata={"artifact_id": artifact["id"], "generated": True},
            )
            db.execute(
                "UPDATE processing_artifacts SET state='ready',file_id=?,item_id=?,publication_snapshot=?,error=NULL WHERE id=?",
                (
                    identifier,
                    output_record["item_id"],
                    encode(snapshot),
                    artifact["id"],
                ),
            )
            db.execute(
                "INSERT INTO content_baselines VALUES (?,?,?,?,?,?)",
                (
                    self.profile,
                    identifier,
                    "sha256",
                    artifact["sha256"],
                    st.st_size,
                    artifact["job_id"],
                ),
            )
            db.execute(
                "INSERT INTO file_facts VALUES (?,?,?,?,?,?,?)",
                (
                    self.profile,
                    identifier,
                    "probe",
                    artifact["job_id"],
                    encode(snapshot),
                    encode(artifact["validation"]["output"]),
                    "complete",
                ),
            )
            self._finish_attempt(
                db,
                artifact["job_id"],
                artifact["attempt"],
                "complete",
                {
                    "artifact_id": artifact["id"],
                    "file_id": identifier,
                    **output_record,
                    "validation": artifact["validation"],
                },
            )
            if self.store.schema_version >= 14:
                from .catalog_refresh import enqueue

                enqueue(db, self.profile)

    def _fail(self, artifact, state, error):
        with self.store.transaction() as db:
            db.execute(
                "UPDATE processing_artifacts SET state=?,error=? WHERE id=?",
                (state, str(error)[:4000], artifact["id"]),
            )
            self._finish_attempt(
                db,
                artifact["job_id"],
                artifact["attempt"],
                "failed",
                error=str(error)[:4000],
            )

    def recover(self, *, limit=100):
        page_limit(limit)
        rows = self.store.rows(
            "SELECT id FROM processing_artifacts WHERE profile=? AND state IN ('planned','writing','validating','publishing') ORDER BY created_at,id LIMIT ?",
            (self.profile, limit),
        )
        recovered, errors = [], []
        for row in rows:
            artifact = self.get(row["id"])
            if artifact["state"] == "publishing":
                try:
                    self._publish(artifact)
                    recovered.append(artifact["id"])
                except (OSError, CatabolicError) as exc:
                    errors.append({"id": artifact["id"], "error": str(exc)})
                    with self.store.transaction() as db:
                        db.execute(
                            "UPDATE processing_artifacts SET error=? WHERE id=?",
                            (str(exc)[:4000], artifact["id"]),
                        )
            else:
                self._fail(
                    artifact,
                    "interrupted",
                    "interrupted before validated publication; retained partial file for inspection",
                )
                recovered.append(artifact["id"])
        from .catalog_refresh import finish

        return finish(
            self.app, {"recovered": recovered, "errors": errors, "complete": not errors}
        )

    def run(self, *, limit=100, job_ids=None, _refresh=True):
        page_limit(limit)
        if job_ids is not None and (
            not isinstance(job_ids, list)
            or len(job_ids) > 1000
            or any(not isinstance(v, str) for v in job_ids)
        ):
            raise CatabolicError("job_ids must contain at most 1000 IDs")
        ids = tuple(sorted(set(job_ids or [])))
        scope = (
            "" if job_ids is None else " AND id IN (" + ",".join("?" for _ in ids) + ")"
        )
        self.app.require_recovered()
        if self.store.rows(
            "SELECT id FROM processing_artifacts WHERE profile=? AND state IN ('planned','writing','validating','publishing') LIMIT 1",
            (self.profile,),
        ):
            raise CatabolicError(
                "run artifact recover before starting more output jobs"
            )
        rows = self.store.rows(
            "SELECT * FROM processing_jobs WHERE profile=? AND operation='render' AND state='queued'"
            + scope
            + " ORDER BY created_at,id LIMIT ?",
            (self.profile, *ids, limit),
        )
        completed, errors = [], []
        for job in rows:
            try:
                completed.append(self._run_one(job))
            except (OSError, CatabolicError, CommandFailure, ValueError) as exc:
                errors.append({"job_id": job["id"], "error": str(exc)[:4000]})
        from .catalog_refresh import finish

        return finish(
            self.app,
            {"completed": completed, "errors": errors, "complete": not errors},
            enabled=_refresh,
        )

    def _run_one(self, job):
        options = json.loads(job["options"])
        recipe = options["recipe"]
        binding = options["destination"]
        identifier = str(uuid4())
        attempt = job["attempts"] + 1
        extension, _, role, _, _ = rendering.PRESETS[recipe["preset"]]
        if options.get("output_definition"):
            role = options["output_definition"]["definition"]["role"]
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO processing_attempts(job_id,attempt,state,retryable,retry_after,started_at) VALUES (?,?,'running',0,0,CURRENT_TIMESTAMP)",
                (job["id"], attempt),
            )
            db.execute(
                "UPDATE processing_jobs SET state='running',attempts=?,error=NULL,finished_at=NULL WHERE id=?",
                (attempt, job["id"]),
            )
            db.execute(
                "INSERT INTO processing_artifacts(id,job_id,attempt,profile,location,path,temporary_path,binding,state,item_id,role) VALUES (?,?,?,?,?,?,?,?,'planned',?,?)",
                (
                    identifier,
                    job["id"],
                    attempt,
                    self.profile,
                    binding["owner"],
                    f"{identifier}.{extension}",
                    f".{identifier}.partial",
                    encode(binding),
                    options["item_id"],
                    role,
                ),
            )
        artifact = self.get(identifier)
        try:
            if options.get("output_definition"):
                Outputs(self.app).validate_source_item(
                    job["file_id"],
                    options["item_id"],
                    options["output_definition"]["definition"],
                )
            if rendering.tools() != options["tools"]:
                raise CatabolicError(
                    "FFmpeg or ffprobe changed since enqueue; submit a new job"
                )
            if self.app.binding("source", binding["owner"]) != binding:
                raise CatabolicError("destination binding changed since enqueue")
            with (
                root_handle(binding) as root,
                validated_source(json.loads(job["snapshot"])) as input_fd,
            ):
                if owner_state(root, self._owner(binding["owner"])) != "owned":
                    raise CatabolicError("generated location ownership changed")
                output_fd = os.open(
                    artifact["temporary_path"],
                    os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                    0o600,
                    dir_fd=root,
                )
                try:
                    st = os.fstat(output_fd)
                    with self.store.transaction() as db:
                        db.execute(
                            "UPDATE processing_artifacts SET state='writing',device=?,inode=? WHERE id=?",
                            (st.st_dev, st.st_ino, identifier),
                        )

                    def poll():
                        space = os.fstatvfs(root)
                        if space.f_bavail * space.f_frsize < recipe["reserve_bytes"]:
                            raise CatabolicError(
                                "generated filesystem is below the free-space reserve"
                            )
                        if os.fstat(output_fd).st_size >= recipe["max_output_bytes"]:
                            raise CatabolicError(
                                "generated output exceeds its byte budget"
                            )

                    poll()
                    validation = rendering.render(
                        input_fd, output_fd, recipe, options["tools"], poll
                    )
                    with self.store.transaction() as db:
                        db.execute(
                            "UPDATE processing_artifacts SET state='validating' WHERE id=?",
                            (identifier,),
                        )
                    checksum = rendering.digest(output_fd)
                    size = os.fstat(output_fd).st_size
                finally:
                    os.close(output_fd)
                os.fsync(root)
            captured = json.loads(job["snapshot"])
            current = occurrence(self.store, self.profile, job["file_id"])
            if any(current.get(k) != v for k, v in captured.items() if k != "ctime_ns"):
                raise CatabolicError(
                    "source or policy changed before publication; enqueue a new job"
                )
            # The source descriptor context validates the input before publication intent is committed.
            with self.store.transaction() as db:
                db.execute(
                    "UPDATE processing_artifacts SET state='publishing',size=?,sha256=?,validation=? WHERE id=?",
                    (size, checksum, encode(validation), identifier),
                )
            self._publish(self.get(identifier))
            output_record = self.store.rows(
                "SELECT id,item_id FROM media_outputs WHERE artifact_id=?",
                (identifier,),
            )[0]
            return {
                "job_id": job["id"],
                "artifact_id": identifier,
                "file_id": self.get(identifier)["file_id"],
                "output_id": output_record["id"],
                "item_id": output_record["item_id"],
            }
        except BaseException as exc:
            artifact = self.get(identifier)
            if artifact["state"] not in ("publishing", "ready"):
                self._fail(
                    artifact,
                    "interrupted"
                    if isinstance(exc, (KeyboardInterrupt, SystemExit))
                    else "failed",
                    exc,
                )
            # Publishing failures retain their intent, including a completed rename.
            raise
