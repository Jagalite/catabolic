# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Entry worklogs and completion gates, evaluated from recorded evidence only."""

import json
from uuid import uuid4

from .curation import bounded_rows, page_limit
from .domain import CatabolicError
from .store import encode

STATUSES = ("pending", "in_progress", "complete", "deferred", "ignored")
KINDS = ("review", "file", "job", "artifact", "proposal")
MAX_NOTE_BYTES = 65536


def _snapshot_matches(snapshot, observation, binding):
    return (
        " AND ".join(
            f"json_extract({snapshot},'$.{field}') IS {observation}.{column}"
            for field, column in (
                ("size", "size"),
                ("mtime_ns", "mtime_ns"),
                ("device", "device"),
                ("inode", "inode"),
            )
        )
        + f" AND json_extract({snapshot},'$.root') IS {binding}.root AND json_extract({snapshot},'$.root_device') IS {binding}.device AND json_extract({snapshot},'$.root_inode') IS {binding}.inode"
    )


# One evaluator supplies CLI, item summaries, SQL and GraphQL. All table names and
# expressions are constants, not user input. Each requirement retains its evidence profile.
ARTIFACT_OK = (
    """a.state='ready' AND ao.status='present' AND
 a.size=ao.size AND a.device=ao.device AND a.inode=ao.inode AND
 ((af.status='complete' AND """
    + _snapshot_matches("a.publication_snapshot", "ao", "ab")
    + ") OR (av.status='complete' AND json_extract(av.data,'$.digest')=a.sha256 AND "
    + _snapshot_matches("av.snapshot", "ao", "ab")
    + "))"
)
JOB_OK = (
    """j.state='complete' AND jo.status='present' AND """
    + _snapshot_matches("j.snapshot", "jo", "jb")
    + " AND ((j.operation!='render' AND jf.status='complete' AND "
    + _snapshot_matches("jf.snapshot", "jo", "jb")
    + ") OR (j.operation='render' AND "
    + ARTIFACT_OK
    + "))"
)
CHECKS_SQL = f"""
SELECT i.id AS item_id,p.id AS profile,'title' AS check_id,
 'metadata' AS kind,'Entry has a title' AS label,'missing' AS state,0 AS satisfied,
 NULL AS target_id,p.id AS evidence_profile
FROM main.items i CROSS JOIN main.profiles p
WHERE coalesce(json_type(i.metadata,'$.title'),'null')!='text'
 OR length(trim(coalesce(json_extract(i.metadata,'$.title'),''),' '||char(9)||char(10)||char(13)))=0
UNION ALL
SELECT f.item_id,p.id,'primary:'||f.id,'file','Active primary file: '||files.path,
 coalesce(o.status,'unknown'),0,f.file_id,p.id
FROM main.item_files f CROSS JOIN main.profiles p
JOIN main.files files ON files.id=f.file_id
LEFT JOIN main.observations o ON o.file_id=f.file_id AND o.profile=p.id
LEFT JOIN main.bindings b ON b.profile=p.id AND b.kind='source' AND b.owner=files.location
WHERE f.active=1 AND f.role='primary' AND (coalesce(o.status,'unknown')!='present' OR b.root IS NULL)
UNION ALL
SELECT r.item_id,p.id,r.id,r.kind,r.label,
 CASE WHEN r.state='waived' THEN 'waived'
 WHEN r.kind='review' THEN r.state
 WHEN r.kind='file' THEN coalesce(fo.status,'unknown')
 WHEN r.kind='job' THEN coalesce(j.state,'unknown')
 WHEN r.kind='artifact' THEN coalesce(a.state,'unknown')
 ELSE coalesce(pr.state,'unknown') END,
 CASE WHEN r.state='waived' THEN 1
 WHEN r.kind='review' THEN r.state='complete'
 WHEN r.kind='file' THEN fo.status='present' AND fb.root IS NOT NULL
 WHEN r.kind='job' THEN {JOB_OK}
 WHEN r.kind='artifact' THEN {ARTIFACT_OK}
 WHEN r.kind='proposal' THEN pr.state='accepted'
 ELSE 0 END,coalesce(r.file_id,r.job_id,r.artifact_id,r.proposal_id),r.profile
FROM main.item_requirements r CROSS JOIN main.profiles p
LEFT JOIN main.files f ON f.id=r.file_id
LEFT JOIN main.observations fo ON fo.file_id=r.file_id AND fo.profile=r.profile
LEFT JOIN main.bindings fb ON fb.profile=r.profile AND fb.kind='source' AND fb.owner=f.location
LEFT JOIN main.processing_jobs j ON j.id=r.job_id
LEFT JOIN main.observations jo ON jo.file_id=j.file_id AND jo.profile=j.profile
LEFT JOIN main.bindings jb ON jb.profile=j.profile AND jb.kind='source' AND jb.owner=j.location
LEFT JOIN main.file_facts jf ON jf.profile=j.profile AND jf.file_id=j.file_id AND jf.operation=j.operation
LEFT JOIN main.processing_artifacts a ON a.id=coalesce(r.artifact_id,
 CASE WHEN r.kind='job' THEN json_extract(j.result,'$.artifact_id') END)
LEFT JOIN main.files afile ON afile.id=a.file_id
LEFT JOIN main.observations ao ON ao.file_id=a.file_id AND ao.profile=a.profile
LEFT JOIN main.bindings ab ON ab.profile=a.profile AND ab.kind='source' AND ab.owner=afile.location
LEFT JOIN main.file_facts af ON af.file_id=a.file_id AND af.profile=a.profile AND af.operation='probe'
LEFT JOIN main.file_facts av ON av.file_id=a.file_id AND av.profile=a.profile AND av.operation='verify'
LEFT JOIN main.proposals pr ON pr.id=r.proposal_id
"""
RULE_CHECKS_SQL = f"""
SELECT r.item_id,p.id AS profile,r.id AS check_id,'rule' AS kind,
 'Rule '||rr.name||' revision '||rr.revision AS label,
 CASE WHEN r.state='waived' THEN 'waived' ELSE coalesce(j.state,'pending') END AS state,
 CASE WHEN r.state='waived' THEN 1 ELSE ({JOB_OK}) END AS satisfied,
 r.rule_id AS target_id,r.profile AS evidence_profile
FROM main.rule_requirements r CROSS JOIN main.profiles p
JOIN main.processing_rules rr ON rr.id=r.rule_id
LEFT JOIN main.processing_jobs j ON j.id=r.job_id
LEFT JOIN main.observations jo ON jo.file_id=j.file_id AND jo.profile=j.profile
LEFT JOIN main.bindings jb ON jb.profile=j.profile AND jb.kind='source' AND jb.owner=j.location
LEFT JOIN main.file_facts jf ON jf.profile=j.profile AND jf.file_id=j.file_id AND jf.operation=j.operation
LEFT JOIN main.processing_artifacts a ON a.id=json_extract(j.result,'$.artifact_id')
LEFT JOIN main.files afile ON afile.id=a.file_id
LEFT JOIN main.observations ao ON ao.file_id=a.file_id AND ao.profile=a.profile
LEFT JOIN main.bindings ab ON ab.profile=a.profile AND ab.kind='source' AND ab.owner=afile.location
LEFT JOIN main.file_facts af ON af.file_id=a.file_id AND af.profile=a.profile AND af.operation='probe'
LEFT JOIN main.file_facts av ON av.file_id=a.file_id AND av.profile=a.profile AND av.operation='verify'
"""
CHECKS_SQL += " UNION ALL " + RULE_CHECKS_SQL
SUMMARY_SQL = f"""SELECT i.id AS item_id,p.id AS profile,
 coalesce(w.status,'pending') AS requested_status,
 CASE WHEN w.status='complete' AND EXISTS(
  SELECT 1 FROM ({CHECKS_SQL}) c WHERE c.item_id=i.id AND c.profile=p.id
  AND coalesce(c.satisfied,0)=0) THEN 'needs_attention'
 ELSE coalesce(w.status,'pending') END AS status,
 coalesce(w.revision,0) AS revision,w.updated_at,w.updated_by
FROM main.items i CROSS JOIN main.profiles p
LEFT JOIN main.item_workflow w ON w.item_id=i.id"""


def text(value, label, maximum=MAX_NOTE_BYTES, *, empty=True):
    if not isinstance(value, str) or "\0" in value or (not empty and not value.strip()):
        raise CatabolicError(
            f"{label} must be {'nonempty ' if not empty else ''}text without NUL characters"
        )
    if len(value.encode()) > maximum:
        raise CatabolicError(f"{label} exceeds {maximum} UTF-8 bytes")
    return value


class ItemWorkflow:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def _item(self, item_id):
        if not self.store.rows("SELECT 1 FROM items WHERE id=?", (item_id,)):
            raise CatabolicError("unknown item")

    def get(self, item_id):
        self._item(item_id)
        return self.store.rows(
            f"SELECT * FROM ({SUMMARY_SQL}) WHERE item_id=? AND profile=?",
            (item_id, self.profile),
        )[0]

    def checks(self, item_id, *, limit=100, after=""):
        self._item(item_id)
        page_limit(limit)
        rows, more = bounded_rows(
            self.store,
            f"SELECT *,coalesce(satisfied,0) AS passed FROM ({CHECKS_SQL}) WHERE item_id=? AND profile=? AND check_id>? ORDER BY check_id LIMIT ?",
            (item_id, self.profile, after, limit + 1),
            limit,
        )
        return {
            "checks": rows,
            "next_after": rows[-1]["check_id"] if more else None,
            "evidence": "recorded state; no live filesystem or media verification",
        }

    def _revision(self, db, item_id, expected):
        self._item(item_id)
        row = db.execute(
            "SELECT * FROM item_workflow WHERE item_id=?", (item_id,)
        ).fetchone()
        revision = row["revision"] if row else 0
        if expected is not None and (type(expected) is not int or expected < 0):
            raise CatabolicError("expected revision must be a nonnegative integer")
        if expected is not None and expected != revision:
            raise CatabolicError(
                f"entry changed: expected revision {expected}, current revision {revision}; read the worklog before retrying"
            )
        return row["status"] if row else "pending", revision

    def _append(self, db, item_id, kind, body, actor, data, status, revision):
        text(body, "worklog text")
        text(actor, "actor", 255, empty=False)
        revision += 1
        db.execute(
            "INSERT INTO item_workflow(item_id,status,revision,updated_by) VALUES (?,?,?,?) ON CONFLICT(item_id) DO UPDATE SET status=excluded.status,revision=excluded.revision,updated_by=excluded.updated_by,updated_at=CURRENT_TIMESTAMP",
            (item_id, status, revision, actor),
        )
        event = db.execute(
            "INSERT INTO item_worklog(item_id,profile,revision,kind,body,actor,data) VALUES (?,?,?,?,?,?,?)",
            (item_id, self.profile, revision, kind, body, actor, encode(data)),
        ).lastrowid
        return {"event_id": event, "revision": revision}

    def note(self, item_id, body, *, kind="note", actor="user", expected_revision=None):
        if kind not in ("note", "decision", "progress"):
            raise CatabolicError("note kind must be note, decision, or progress")
        text(body, "worklog text", empty=False)
        with self.store.transaction() as db:
            status, revision = self._revision(db, item_id, expected_revision)
            result = self._append(db, item_id, kind, body, actor, {}, status, revision)
        return {**result, "workflow": self.get(item_id)}

    def set_status(
        self, item_id, status, *, note="", actor="user", expected_revision=None
    ):
        if status not in STATUSES:
            raise CatabolicError(
                "invalid entry status; needs_attention is computed from completion checks"
            )
        if status == "complete":
            self.app.require_recovered()
        with self.store.transaction() as db:
            before, revision = self._revision(db, item_id, expected_revision)
            if status == "complete":
                blockers = self.store.rows(
                    f"SELECT check_id,label,state FROM ({CHECKS_SQL}) WHERE item_id=? AND profile=? AND coalesce(satisfied,0)=0 ORDER BY check_id LIMIT 11",
                    (item_id, self.profile),
                )
                if blockers:
                    detail = "; ".join(
                        f"{r['label']} ({r['state']}; missing or stale evidence)"
                        for r in blockers[:10]
                    )
                    raise CatabolicError(
                        "cannot mark entry complete: "
                        + detail
                        + ("; more blockers exist" if len(blockers) > 10 else "")
                    )
            result = self._append(
                db,
                item_id,
                "status",
                note,
                actor,
                {"before": before, "after": status},
                status,
                revision,
            )
        return {**result, "workflow": self.get(item_id)}

    def log(self, item_id, *, limit=100, after=0):
        self._item(item_id)
        page_limit(limit)
        if type(after) is not int or after < 0:
            raise CatabolicError("after must be a nonnegative event ID")
        rows, more = bounded_rows(
            self.store,
            "SELECT * FROM item_worklog WHERE item_id=? AND id>? ORDER BY id LIMIT ?",
            (item_id, after, limit + 1),
            limit,
        )
        for row in rows:
            row["data"] = json.loads(row["data"])
        return {"entries": rows, "next_after": rows[-1]["id"] if more else None}

    def require(
        self,
        item_id,
        kind,
        label,
        *,
        target=None,
        actor="user",
        note="",
        expected_revision=None,
    ):
        if kind not in KINDS:
            raise CatabolicError("unknown requirement kind")
        text(label, "requirement label", 255, empty=False)
        if (kind == "review") != (target is None):
            raise CatabolicError(
                "review requirements have no target; other requirements need a target ID"
            )
        with self.store.transaction() as db:
            status, revision = self._revision(db, item_id, expected_revision)
            if (
                db.execute(
                    "SELECT count(*) FROM item_requirements WHERE item_id=?", (item_id,)
                ).fetchone()[0]
                >= 1000
            ):
                raise CatabolicError("entry exceeds 1000 requirements")
            field, file_id = None, None
            if kind != "review":
                table, field = {
                    "file": ("files", "file_id"),
                    "job": ("processing_jobs", "job_id"),
                    "artifact": ("processing_artifacts", "artifact_id"),
                    "proposal": ("proposals", "proposal_id"),
                }[kind]
                rows = self.store.rows(f"SELECT * FROM {table} WHERE id=?", (target,))
                if not rows or kind != "file" and rows[0]["profile"] != self.profile:
                    raise CatabolicError("unknown requirement target in this profile")
                row = rows[0]
                file_id = row["id"] if kind == "file" else row.get("file_id")
                if kind == "artifact":
                    belongs = row["item_id"] == item_id
                else:
                    belongs = bool(
                        db.execute(
                            "SELECT 1 FROM item_files WHERE item_id=? AND file_id=?",
                            (item_id, file_id),
                        ).fetchone()
                    )
                if not belongs:
                    raise CatabolicError(
                        "requirement target does not belong to this entry"
                    )
            identifier = str(uuid4())
            fields = (item_id, self.profile, kind, label)
            if field:
                db.execute(
                    f"INSERT INTO item_requirements(id,item_id,profile,kind,label,state,{field}) VALUES (?,?,?,?,?,'pending',?)",
                    (identifier, *fields, target),
                )
            else:
                db.execute(
                    "INSERT INTO item_requirements(id,item_id,profile,kind,label,state) VALUES (?,?,?,?,?,'pending')",
                    (identifier, *fields),
                )
            result = self._append(
                db,
                item_id,
                "requirement",
                note,
                actor,
                {
                    "action": "add",
                    "id": identifier,
                    "kind": kind,
                    "label": label,
                    "target": target,
                },
                status,
                revision,
            )
        return {**result, "requirement_id": identifier, "workflow": self.get(item_id)}

    def resolve(
        self, requirement_id, state, *, note, actor="user", expected_revision=None
    ):
        if state not in ("pending", "complete", "waived"):
            raise CatabolicError(
                "requirement state must be pending, complete, or waived"
            )
        text(note, "resolution note", empty=False)
        with self.store.transaction() as db:
            row = db.execute(
                "SELECT * FROM item_requirements WHERE id=?", (requirement_id,)
            ).fetchone()
            table = "item_requirements"
            if row is None:
                row = db.execute(
                    "SELECT *,'rule' AS kind FROM rule_requirements WHERE id=?",
                    (requirement_id,),
                ).fetchone()
                table = "rule_requirements"
            if row is None:
                raise CatabolicError("unknown requirement")
            if row["kind"] != "review" and state == "complete":
                raise CatabolicError(
                    "file/job/artifact/proposal requirements are evaluated from evidence; they cannot be manually completed"
                )
            status, revision = self._revision(db, row["item_id"], expected_revision)
            db.execute(
                f"UPDATE {table} SET state=? WHERE id=?",
                (state, requirement_id),
            )
            result = self._append(
                db,
                row["item_id"],
                "requirement",
                note,
                actor,
                {
                    "action": "resolve",
                    "id": requirement_id,
                    "before": row["state"],
                    "after": state,
                },
                status,
                revision,
            )
        return {**result, "workflow": self.get(row["item_id"])}
