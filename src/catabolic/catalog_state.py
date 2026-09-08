# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Normalized recorded evidence, shared by SQL and GraphQL. Never live I/O."""

from .outputs import RENDITIONS_SQL
from .revision_evidence import _snapshot_matches

SOURCE = "coalesce(j.snapshot,json_extract(e.payload,'$.source.revision'))"
OUTPUT = "coalesce(a.publication_snapshot,ro.snapshot)"
RECEIPT_SOURCE = " AND ".join(
    f"json_extract({SOURCE},'$.{field}') IS so.{field}"
    for field in ("size", "mtime_ns", "device", "inode")
)

RENDITION_STATE_SQL = f"""SELECT state.*,
 CASE WHEN evidence_kind='declared' THEN NULL
 ELSE source_current AND output_current AND accepted END AS recorded_current
 FROM (SELECT r.*,
 CASE WHEN r.artifact_id IS NOT NULL THEN 'artifact'
 WHEN ro.output_id IS NOT NULL THEN 'receipt' ELSE 'declared' END AS evidence_kind,
 {SOURCE} AS source_revision,{OUTPUT} AS output_revision,
 CASE WHEN {SOURCE} IS NULL THEN NULL ELSE
 coalesce(so.status='present' AND CASE WHEN j.snapshot IS NOT NULL
 THEN {_snapshot_matches(SOURCE, "so", "sb")}
 ELSE sb.root IS NOT NULL AND {RECEIPT_SOURCE} END,0) END AS source_current,
 CASE WHEN {OUTPUT} IS NULL THEN NULL ELSE
 coalesce(oo.status='present' AND (({_snapshot_matches(OUTPUT, "oo", "ob")}) OR
 (vf.status='complete' AND json_extract(vf.data,'$.digest')=coalesce(a.sha256,ro.sha256)
 AND {_snapshot_matches("vf.snapshot", "oo", "ob")})),0) END AS output_current,
 CASE WHEN r.artifact_id IS NOT NULL THEN a.state='ready' AND j.state='complete'
 WHEN ro.output_id IS NOT NULL THEN 1 ELSE 0 END AS accepted,
 so.status AS source_status,oo.status AS output_status,
 coalesce(a.sha256,ro.sha256) AS verified_sha256,
 coalesce(r.recipe_id,pj.operation_id) AS operation_id,pr.revision AS operation_revision,pr.operation_kind,
 CASE WHEN pf.status='complete' AND {_snapshot_matches("pf.snapshot", "oo", "ob")}
 THEN json_extract(pf.data,'$.summary') END AS technical
 FROM ({RENDITIONS_SQL}) r
 LEFT JOIN main.processing_artifacts a ON a.id=r.artifact_id
 LEFT JOIN main.processing_jobs j ON j.id=a.job_id
 LEFT JOIN main.processor_jobs pj ON pj.receipt_id=(SELECT receipt_id FROM main.receipt_outputs WHERE output_id=r.id)
 LEFT JOIN main.processing_recipes pr ON pr.id=coalesce(r.recipe_id,pj.operation_id)
 LEFT JOIN main.receipt_outputs ro ON ro.output_id=r.id
 LEFT JOIN main.external_receipts e ON e.id=ro.receipt_id
 LEFT JOIN main.files sf ON sf.id=r.source_file_id
 LEFT JOIN main.observations so ON so.file_id=sf.id AND so.profile=r.profile
 LEFT JOIN main.bindings sb ON sb.profile=r.profile AND sb.kind='source' AND sb.owner=sf.location
 LEFT JOIN main.files ofile ON ofile.id=r.file_id
 LEFT JOIN main.observations oo ON oo.file_id=ofile.id AND oo.profile=r.profile
 LEFT JOIN main.bindings ob ON ob.profile=r.profile AND ob.kind='source' AND ob.owner=ofile.location
 LEFT JOIN main.file_facts vf ON vf.profile=r.profile AND vf.file_id=r.file_id AND vf.operation='verify'
 LEFT JOIN main.file_facts pf ON pf.profile=r.profile AND pf.file_id=r.file_id AND pf.operation='probe'
 ) state"""


def rendition_condition(value, *, alias="r"):
    """Parameterized, typed EXISTS filter, with unknown currentness excluded."""
    from .domain import CatabolicError

    if not isinstance(value, dict) or set(value) - {
        "purpose",
        "recipe",
        "definition",
        "current",
    }:
        raise CatabolicError(
            "rendition filter accepts purpose, recipe, definition and current"
        )
    clauses, values = [], []
    for key, column in (
        ("purpose", "purpose"),
        ("recipe", "operation_id"),
        ("definition", "definition_id"),
        ("current", "recorded_current"),
    ):
        if key in value:
            if key == "current":
                if type(value[key]) is not bool:
                    raise CatabolicError("rendition current must be boolean")
            elif not isinstance(value[key], str) or not value[key]:
                raise CatabolicError(
                    "rendition filter identifiers must be nonempty strings"
                )
            clauses.append(f"{alias}.{column}=?")
            values.append(value[key])
    return clauses, values
