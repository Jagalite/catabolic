# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Bounded, read-only SQL for humans and command-line agents."""

from __future__ import annotations

import json
import math
import re
import sqlite3
import time
from contextlib import nullcontext

from .catalog_state import RENDITION_STATE_SQL
from .domain import CatabolicError
from .item_workflow import CHECKS_SQL, SUMMARY_SQL
from .outputs import RENDITIONS_SQL
from .store import Store

INTERFACE_VERSION = 1
MAX_SQL_BYTES = 1024 * 1024
MAX_RESULT_BYTES = 8 * 1024 * 1024

# Connection-local views are an API independent of the stored schema. Qualifying
# all base tables prevents accidental name resolution through a temporary object.
VIEWS = {
    "catalog_rule_processor_jobs": (
        "Rule provenance for existing fenced external processor jobs.",
        "SELECT * FROM main.rule_processor_jobs",
    ),
    "catalog_queries": (
        "Immutable saved query revisions; dependencies pin IDs.",
        "SELECT * FROM main.saved_queries",
    ),
    "catalog_query_dependencies": (
        "Pinned query composition edges.",
        "SELECT * FROM main.query_dependencies",
    ),
    "catalog_projections": (
        "Configured query and layout per profile/catalog; mappings and owned links record execution separately.",
        "SELECT * FROM main.projection_bindings",
    ),
    "catalog_rendition_state": (
        "Recorded source/output revision currentness and acceptance; NULL means evidence unknown. Does not replace live publication validation or establish ancestor currentness.",
        RENDITION_STATE_SQL,
    ),
    "catalog_items": (
        "One row per media item, including items with no mappings. Metadata is JSON text.",
        """SELECT id AS item_id, kind, catalog_title(metadata) AS title,
        catalog_year(metadata) AS year, metadata FROM main.items""",
    ),
    "catalog_identities": (
        "One row per provider identity. An item may have several identities.",
        "SELECT item_id, namespace, value FROM main.identities",
    ),
    "catalog_files": (
        "One row per profile and inventoried file. Status is recorded availability, not a live check.",
        """SELECT p.id AS profile, f.id AS file_id, f.location,
        f.path AS source_relative_path, b.root AS source_root,
        CASE WHEN b.root IS NOT NULL THEN rtrim(b.root,'/') || '/' || f.path END AS source_path,
        o.size, o.mtime_ns, o.device, o.inode, b.device AS root_device, b.inode AS root_inode, coalesce(o.status,'unknown') AS status,
        o.scan_id, s.finished_at AS observed_at
        FROM main.profiles p CROSS JOIN main.files f
        LEFT JOIN main.observations o ON o.profile=p.id AND o.file_id=f.id
        LEFT JOIN main.scans s ON s.id=o.scan_id
        LEFT JOIN main.bindings b ON b.profile=p.id AND b.kind='source' AND b.owner=f.location""",
    ),
    "catalog_entries": (
        "One row per profile and mapping, including disabled mappings. Filter active=1 for current decisions.",
        """SELECT cf.profile, m.id AS mapping_id, m.catalog, m.active,
        m.item_id, i.kind, i.title, i.year, m.file_id,
        cf.location, cf.source_relative_path, cf.source_root, cf.source_path,
        cf.size, cf.mtime_ns, cf.status, cf.scan_id, cf.observed_at,
        m.path AS catalog_path, b.root AS output_root,
        CASE WHEN b.root IS NOT NULL THEN rtrim(b.root,'/') || '/' || m.path END AS output_path,
        ol.target AS recorded_link_target
        FROM main.mappings m JOIN catalog_files cf ON cf.file_id=m.file_id
        JOIN catalog_items i ON i.item_id=m.item_id
        LEFT JOIN main.bindings b ON b.profile=cf.profile AND b.kind='output' AND b.owner=m.catalog
        LEFT JOIN main.owned_links ol ON ol.profile=cf.profile AND ol.catalog=m.catalog AND ol.path=m.path""",
    ),
}

VIEWS.update(
    {
        "catalog_item_workflow": (
            "One item/profile readiness summary; requested status is shared, needs_attention is derived from recorded blockers.",
            SUMMARY_SQL,
        ),
        "catalog_workflow_checks": (
            "Required checks and automatic blockers. Filter coalesce(satisfied,0)=0 for blockers. No live verification.",
            CHECKS_SQL,
        ),
        "catalog_item_requirements": (
            "Explicit entry requirements; automatic requirements are evaluated from their recorded evidence profile.",
            "SELECT * FROM main.item_requirements",
        ),
        "catalog_item_worklog": (
            "Append-only entry notes and workflow events, ordered by id. Actor is supplied attribution, not authenticated identity.",
            "SELECT * FROM main.item_worklog",
        ),
        "catalog_artifacts": (
            "Generated files and publication state, with input lineage and recipe revision.",
            "SELECT a.*,j.file_id AS input_file_id,j.snapshot AS input_snapshot,j.recipe_id FROM main.processing_artifacts a JOIN main.processing_jobs j ON j.id=a.job_id",
        ),
        "catalog_recipes": (
            "Immutable processing recipe revisions.",
            "SELECT * FROM main.processing_recipes",
        ),
        "catalog_rules": (
            "Saved immutable processing rule revisions and maintenance enablement.",
            "SELECT * FROM main.processing_rules",
        ),
        "catalog_rule_jobs": (
            "Rule-to-job provenance, including jobs retained from old revisions.",
            "SELECT r.*,j.state FROM main.rule_jobs r JOIN main.processing_jobs j ON j.id=r.job_id",
        ),
        "catalog_renditions": (
            "Registered and generated media outputs with source lineage and immutable definition IDs; registered provenance is user-declared.",
            RENDITIONS_SQL,
        ),
        "catalog_rendition_policies": (
            "Catalog-scoped rendition admission policies.",
            "SELECT * FROM main.rendition_policies",
        ),
        "catalog_processors": (
            "Immutable HTTP receipt processor definitions; secrets are environment references.",
            "SELECT * FROM main.processors",
        ),
        "catalog_processor_jobs": (
            "External submission state and worker generations; capability tokens omitted.",
            "SELECT id,profile,processor,request,digest,state,generation,worker,lease_until,receipt_id,error,created_at,updated_at FROM main.processor_jobs",
        ),
        "catalog_processor_attempts": (
            "Durable worker attempts including expired generations.",
            "SELECT * FROM main.processor_attempts",
        ),
        "catalog_rendition_decisions": (
            "Explicit per-catalog rendition exclusions and reasons.",
            "SELECT * FROM main.rendition_decisions",
        ),
        "catalog_external_receipts": (
            "Accepted producer claims; delivered bytes are independently validated.",
            "SELECT * FROM main.external_receipts",
        ),
        "catalog_receipt_outputs": (
            "Receipt output references with verified hashes and file snapshots.",
            "SELECT * FROM main.receipt_outputs",
        ),
        "catalog_rule_requirements": (
            "Semantic rendition requirements and their current job evidence.",
            "SELECT * FROM main.rule_requirements",
        ),
        "catalog_rule_evaluations": (
            "Complete recorded evaluations of required rules.",
            "SELECT * FROM main.rule_evaluations",
        ),
        "catalog_output_definitions": (
            "Immutable rendition metadata and item relationship policies shared by recipes and external registration.",
            "SELECT * FROM main.output_definitions",
        ),
        "catalog_generated_locations": (
            "Explicitly owned storage locations for generated files.",
            "SELECT * FROM main.generated_locations",
        ),
        "catalog_item_files": (
            "One row per profile and file identification, independent of catalog placement; includes disabled associations.",
            """SELECT cf.profile,a.id AS association_id,a.item_id,i.kind,i.title,i.year,
        a.file_id,a.role,a.part,a.metadata,a.origin,a.active,
        cf.location,cf.source_relative_path,cf.source_root,cf.source_path,
        cf.size,cf.mtime_ns,cf.status,cf.scan_id,cf.observed_at
        FROM main.item_files a JOIN catalog_files cf ON cf.file_id=a.file_id
        JOIN catalog_items i ON i.item_id=a.item_id""",
        ),
        "catalog_relationships": (
            "One row per directed item relationship across all profiles; includes disabled relationships.",
            """SELECT r.id AS relationship_id,r.source_id,s.kind AS source_kind,
        s.title AS source_title,r.target_id,t.kind AS target_kind,t.title AS target_title,
        r.kind,r.position,r.metadata,r.active FROM main.item_relationships r
        JOIN catalog_items s ON s.item_id=r.source_id JOIN catalog_items t ON t.item_id=r.target_id""",
        ),
    }
)

VIEWS.update(
    {
        "catalog_outputs": (
            "Catalog-wide output link modes.",
            "SELECT c.id AS catalog,coalesce(m.mode,'symlink') AS link_mode FROM main.catalogs c LEFT JOIN main.catalog_link_modes m ON m.catalog=c.id",
        ),
        "catalog_hardlinks": (
            "Recorded hardlink ownership; target is JSON inode/source evidence, not a symlink target.",
            "SELECT profile,catalog,path,target FROM main.owned_hardlinks",
        ),
        "catalog_retained_hardlinks": (
            "Recorded retained data; never automatically purged.",
            "SELECT * FROM main.retained_hardlinks",
        ),
        "catalog_tags": (
            "Canonical tag vocabulary, shared across profiles.",
            "SELECT id AS tag_id,name,description FROM main.tags",
        ),
        "catalog_tag_names": (
            "Canonical names and aliases resolve to the same tag ID.",
            "SELECT n.name,n.tag_id,n.name=t.name AS canonical FROM main.tag_names n JOIN main.tags t ON t.id=n.tag_id",
        ),
        "catalog_tag_parents": (
            "Direct child-to-parent edges; descendants are opt-in recursive queries.",
            "SELECT child_id,parent_id FROM main.tag_parents",
        ),
        "catalog_taggings": (
            "Explicit item/file assertions, including withdrawn ones. Filter active=1; each source is independent.",
            """SELECT a.id AS assignment_id,'item' AS subject_type,a.item_id AS subject_id,a.tag_id,t.name AS tag_name,a.source,a.confidence,a.note,a.active,a.created_at,a.updated_at
        FROM main.item_tags a JOIN main.tags t ON t.id=a.tag_id UNION ALL
        SELECT a.id,'file',a.file_id,a.tag_id,t.name,a.source,a.confidence,a.note,a.active,a.created_at,a.updated_at FROM main.file_tags a JOIN main.tags t ON t.id=a.tag_id""",
        ),
    }
)

# Only known computational functions are permitted. In particular, extension,
# file, and shell functions cannot be called even if a build provides them.
FUNCTIONS = frozenset(
    """
abs avg char coalesce concat concat_ws count format glob group_concat hex if ifnull
iif instr length like likelihood likely lower ltrim max min nullif octet_length
printf quote random randomblob replace round rtrim sign soundex sqlite_source_id
sqlite_version string_agg substr substring sum total trim typeof unicode unlikely
unhex upper zeroblob date time datetime julianday unixepoch strftime timediff
row_number rank dense_rank percent_rank cume_dist ntile lag lead first_value
last_value nth_value json json_array json_array_length json_error_position
json_extract json_group_array json_group_object json_insert json_object json_patch
json_quote json_remove json_replace json_set json_type json_valid -> ->>
jsonb jsonb_array jsonb_extract jsonb_group_array jsonb_group_object jsonb_insert
jsonb_object jsonb_patch jsonb_remove jsonb_replace jsonb_set
acos acosh asin asinh atan atan2 atanh ceil ceiling cos cosh degrees exp floor
ln log log10 log2 mod pi pow power radians sin sinh sqrt tan tanh trunc
catalog_title catalog_year casefold
""".split()
)


VIEWS.update(
    {
        "catalog_jobs": (
            "Durable optional processing jobs and their outcomes.",
            "SELECT * FROM main.processing_jobs",
        ),
        "catalog_job_attempts": (
            "Processing attempt outcomes and transient retry eligibility.",
            "SELECT a.*,j.profile,j.file_id FROM main.processing_attempts a JOIN main.processing_jobs j ON j.id=a.job_id",
        ),
        "catalog_proposals": (
            "Identification and association proposals, including evidence and decisions.",
            "SELECT * FROM main.proposals",
        ),
        "catalog_decisions": (
            "Append-only proposal decision history.",
            "SELECT e.*,p.profile,p.file_id FROM main.decision_events e JOIN main.proposals p ON p.id=e.proposal_id",
        ),
        "catalog_expected": (
            "Versioned expected member sets; choose an explicit set ID.",
            "SELECT * FROM main.expected_sets",
        ),
        "catalog_checksums": (
            "Preserved full-content checksum baselines; not a live integrity assertion.",
            "SELECT * FROM main.content_baselines",
        ),
        "catalog_text": (
            "Extracted text with locator JSON and processing provenance.",
            "SELECT * FROM main.text_segments",
        ),
        "catalog_refresh": (
            "Application refresh delivery status; credentials are environment references.",
            "SELECT * FROM main.refresh_events",
        ),
        "catalog_facts": (
            "Latest successful facts and whether their revision matches recorded inventory; not a live check.",
            """SELECT f.*,
        coalesce(f.status='complete' AND o.status='present' AND o.size=json_extract(f.snapshot,'$.size')
          AND o.mtime_ns=json_extract(f.snapshot,'$.mtime_ns') AND o.inode=json_extract(f.snapshot,'$.inode')
          AND o.device=json_extract(f.snapshot,'$.device') AND b.root=json_extract(f.snapshot,'$.root')
          AND b.device=json_extract(f.snapshot,'$.root_device') AND b.inode=json_extract(f.snapshot,'$.root_inode'),0) AS current,
        json_extract(f.data,'$.summary.width') AS width, json_extract(f.data,'$.summary.height') AS height,
        json_extract(f.data,'$.summary.hdr') AS hdr, json_extract(f.data,'$.summary.duration') AS duration
        FROM main.file_facts f JOIN main.files src ON src.id=f.file_id
        LEFT JOIN main.observations o ON o.profile=f.profile AND o.file_id=f.file_id
        LEFT JOIN main.bindings b ON b.profile=f.profile AND b.kind='source' AND b.owner=src.location""",
        ),
    }
)


def parameters(raw: str | None) -> dict:
    try:
        result = json.loads(raw) if raw is not None else {}
        if not isinstance(result, dict):
            raise ValueError()
        for key, value in result.items():
            if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", key) or key == "profile":
                raise ValueError()
            if value is not None and type(value) not in (str, int, float, bool):
                raise ValueError()
            if type(value) is int and not -(2**63) <= value < 2**63:
                raise ValueError()
            if type(value) is float and not math.isfinite(value):
                raise ValueError()
        return result
    except (ValueError, TypeError) as exc:
        raise CatabolicError(
            "--params must be a JSON object with named scalar values; profile is reserved, numbers must be finite and integers fit signed 64 bits"
        ) from exc


def _field(raw, key, expected):
    value = json.loads(raw).get(key)
    return value if type(value) is expected else None


def _setup(db):
    # Repeated selections reuse temporary views. Changing temp_store after
    # those views exist is unsafe inside a transaction; set it only once.
    if db.execute("PRAGMA temp_store").fetchone()[0] != 2:
        db.execute("PRAGMA temp_store=MEMORY")
    db.create_function(
        "catalog_title", 1, lambda raw: _field(raw, "title", str), deterministic=True
    )
    db.create_function(
        "catalog_year",
        1,
        lambda raw: (
            year
            if (year := _field(raw, "year", int)) is not None and 1 <= year <= 9999
            else None
        ),
        deterministic=True,
    )
    db.create_function(
        "casefold",
        1,
        lambda value: value.casefold() if isinstance(value, str) else None,
        deterministic=True,
    )
    for view, (_, sql) in VIEWS.items():
        db.execute(f"CREATE TEMP VIEW IF NOT EXISTS {view} AS {sql}")
    db.execute("PRAGMA query_only=ON")


def _schema(db, profile):
    def columns(database, name):
        quoted = '"' + name.replace('"', '""') + '"'
        return [
            {"name": row[1], "declared_type": row[2] or None}
            for row in db.execute(f"PRAGMA {database}.table_info({quoted})")
        ]

    return {
        "interface_version": INTERFACE_VERSION,
        "profile": profile,
        "views": [
            {"name": view, "description": description, "columns": columns("temp", view)}
            for view, (description, _) in VIEWS.items()
        ],
        "tables": [
            {"name": row[0], "columns": columns("main", row[0])}
            for row in db.execute(
                "SELECT name FROM main.sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
            )
        ],
        "parameters": {
            "profile": "Automatically bound to the selected CLI profile. Views contain every profile; use WHERE profile=:profile."
        },
        "functions": sorted(
            {row[0] for row in db.execute("PRAGMA function_list")} & FUNCTIONS
        ),
        "sqlite_version": sqlite3.sqlite_version,
        "limits": {
            "default_max_rows": 1000,
            "maximum_max_rows": 10000,
            "default_timeout_ms": 5000,
            "maximum_timeout_ms": 60000,
            "max_sql_bytes": MAX_SQL_BYTES,
            "max_result_bytes": MAX_RESULT_BYTES,
        },
    }


def _cell(value):
    if isinstance(value, bytes):
        return {"$blob": value.hex()}
    if isinstance(value, float) and not math.isfinite(value):
        return {"$float": "Infinity" if value > 0 else "-Infinity"}
    return value


def execute_sql(
    path,
    sql: str | None = None,
    *,
    profile="default",
    params=None,
    describe=False,
    max_rows=1000,
    timeout_ms=5000,
    _store=None,
    _stable=False,
):
    """Read-only SQL; internal selectors may reuse a locked catalog snapshot."""
    if type(max_rows) is not int or not 1 <= max_rows <= 10000:
        raise CatabolicError("max-rows must be between 1 and 10000")
    if type(timeout_ms) is not int or not 1 <= timeout_ms <= 60000:
        raise CatabolicError("timeout-ms must be between 1 and 60000")
    bound = parameters(params)
    if describe:
        if sql is not None or params is not None:
            raise CatabolicError("--schema cannot be combined with SQL or --params")
    elif not sql or not sql.strip():
        raise CatabolicError(
            "provide one SQL statement, --file PATH, --file -, or --schema"
        )
    elif len(sql.encode("utf-8")) > MAX_SQL_BYTES:
        raise CatabolicError("SQL exceeds the 1 MiB input limit")
    bound["profile"] = profile
    with nullcontext(_store) if _store is not None else Store(path) as store:
        db = store.db
        if not db.execute("SELECT 1 FROM profiles WHERE id=?", (profile,)).fetchone():
            raise CatabolicError(f"unknown profile: {profile}")
        previous_query_only = db.execute("PRAGMA query_only").fetchone()[0]
        previous_busy_timeout = db.execute("PRAGMA busy_timeout").fetchone()[0]
        previous_limits = {}
        try:
            _setup(db)
            if describe:
                return _schema(db, profile)
            for category, limit in (
                (sqlite3.SQLITE_LIMIT_SQL_LENGTH, MAX_SQL_BYTES),
                (sqlite3.SQLITE_LIMIT_LENGTH, MAX_SQL_BYTES),
                (sqlite3.SQLITE_LIMIT_COLUMN, 256),
                (sqlite3.SQLITE_LIMIT_EXPR_DEPTH, 100),
                (sqlite3.SQLITE_LIMIT_COMPOUND_SELECT, 50),
                (sqlite3.SQLITE_LIMIT_ATTACHED, 0),
                (sqlite3.SQLITE_LIMIT_VARIABLE_NUMBER, 1000),
            ):
                previous_limits[category] = db.setlimit(category, limit)
            db.execute(f"PRAGMA busy_timeout={min(timeout_ms, 5000)}")
            denied = []

            def authorize(action, arg1, arg2, database, _trigger):
                if action in (sqlite3.SQLITE_SELECT, sqlite3.SQLITE_RECURSIVE):
                    return sqlite3.SQLITE_OK
                if action == sqlite3.SQLITE_READ and database in ("main", "temp"):
                    return sqlite3.SQLITE_OK
                # SQLite can omit the database name for a count(*) over a
                # compound view. Allow only our registered views and only this
                # column-less read; underlying reads/functions are still checked.
                if (
                    action == sqlite3.SQLITE_READ
                    and database is None
                    and arg1 in VIEWS
                    and arg2 == ""
                ):
                    return sqlite3.SQLITE_OK
                if (
                    action == sqlite3.SQLITE_FUNCTION
                    and (arg2 or "").lower() in FUNCTIONS
                    and not (
                        _stable
                        and (arg2 or "").lower()
                        in {
                            "random",
                            "randomblob",
                            "date",
                            "time",
                            "datetime",
                            "julianday",
                            "unixepoch",
                            "strftime",
                            "timediff",
                            "changes",
                            "total_changes",
                            "last_insert_rowid",
                        }
                    )
                ):
                    return sqlite3.SQLITE_OK
                denied.append(
                    arg2 if action == sqlite3.SQLITE_FUNCTION else arg1 or str(action)
                )
                return sqlite3.SQLITE_DENY

            deadline = time.monotonic() + timeout_ms / 1000
            db.set_authorizer(authorize)
            db.set_progress_handler(lambda: int(time.monotonic() >= deadline), 1000)
            cursor = None
            try:
                cursor = db.execute(sql, bound)
                if cursor.description is None:
                    raise CatabolicError("SQL must return a result set")
                columns = [column[0] for column in cursor.description]
                rows = []
                used_bytes = len(
                    json.dumps(columns, ensure_ascii=False).encode("utf-8")
                )
                if used_bytes > MAX_RESULT_BYTES:
                    raise CatabolicError(
                        "result column names exceed the output byte limit"
                    )
                reason = None
                for row in cursor:
                    if time.monotonic() >= deadline:
                        raise CatabolicError(
                            "SQL execution timed out; simplify the query or increase --timeout-ms"
                        )
                    if len(rows) == max_rows:
                        reason = "max_rows"
                        break
                    converted = [_cell(value) for value in row]
                    used_bytes += len(
                        json.dumps(
                            converted, ensure_ascii=False, allow_nan=False
                        ).encode("utf-8")
                    )
                    if used_bytes > MAX_RESULT_BYTES:
                        reason = "max_result_bytes"
                        break
                    rows.append(converted)
                return {
                    "interface_version": INTERFACE_VERSION,
                    "profile": profile,
                    "columns": columns,
                    "rows": rows,
                    "row_count": len(rows),
                    "max_rows": max_rows,
                    "truncated": reason is not None,
                    "truncation_reason": reason,
                    "complete": reason is None,
                }
            except sqlite3.Error as exc:
                if denied:
                    raise CatabolicError(
                        f"read-only SQL rejected an operation or function: {denied[0]}"
                    ) from exc
                if time.monotonic() >= deadline:
                    raise CatabolicError(
                        "SQL execution timed out; simplify the query or increase --timeout-ms"
                    ) from exc
                raise CatabolicError(f"SQL query failed: {exc}") from exc
            finally:
                if cursor is not None:
                    cursor.close()
                db.set_progress_handler(None, 0)
                db.set_authorizer(None)

        finally:
            db.set_progress_handler(None, 0)
            db.set_authorizer(None)
            for category, value in previous_limits.items():
                db.setlimit(category, value)
            db.execute(f"PRAGMA query_only={previous_query_only}")
            db.execute(f"PRAGMA busy_timeout={previous_busy_timeout}")


def render_table(result):
    """Escape cells to keep control characters and embedded newlines on one line."""

    abbreviated = False

    def display(value):
        nonlocal abbreviated
        if value is None:
            return "NULL"
        text = json.dumps(value, ensure_ascii=True, allow_nan=False)
        if len(text) > 160:
            abbreviated = True
            return text[:157] + "..."
        return text

    headers = [display(value) for value in result["columns"]]
    rows = [[display(value) for value in row] for row in result["rows"]]

    def line(row):
        # Avoid padding every short cell to the width of a large value.
        return " | ".join(row)

    return "\n".join(
        [
            line(headers),
            " | ".join("---" for _ in headers),
            *(line(row) for row in rows),
            f"{result['row_count']} row(s)"
            + (
                f"; truncated ({result['truncation_reason']})"
                if result["truncated"]
                else ""
            ),
            *(
                ["Long cells abbreviated; use --format json for full values."]
                if abbreviated
                else []
            ),
        ]
    )
