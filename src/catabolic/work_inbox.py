# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Rebuildable work discovery over authoritative catalog records, never a task store."""

import hashlib
import json

from .domain import CatabolicError
from .revision_evidence import _snapshot_matches
from .store import encode

ACTIONABLE = "('ready','blocked','needs_decision')"
JSON_FIELDS = ("related_work", "preconditions", "suggested_actions", "evidence")


def branch(source, **fields):
    columns = dict(
        work_key="NULL",
        profile="NULL",
        evidence_profile="NULL",
        subject_kind="NULL",
        subject_id="NULL",
        source_kind="NULL",
        source_id="NULL",
        category="NULL",
        label="NULL",
        reason="NULL",
        source_status="NULL",
        actionability="NULL",
        required="NULL",
        countable="1",
        related_work="'[]'",
        evidence_at="NULL",
        evidence_complete="NULL",
        current="NULL",
        evidence="'{}'",
        preconditions="'{}'",
        suggested_actions="'[]'",
    )
    columns.update(fields)
    return (
        "SELECT "
        + ",".join(f"{value} AS {key}" for key, value in columns.items())
        + " FROM "
        + source
    )


def action(operation, arguments="json_object()", *, note=False):
    # Operation names are code constants. Arguments are JSON values, never shell text.
    return f"json_array(json_object('operation','{operation}','arguments',{arguments},'reload_required',json('true'),'note_required',json('{str(note).lower()}')))"


def owner_pause(owners):
    """A required owner's explicit deferral also applies to its linked detail row."""
    source = f"({owners}) ow LEFT JOIN main.item_workflow iw ON iw.item_id=ow.item_id"
    return (
        f" WHEN EXISTS(SELECT 1 FROM ({owners})) AND NOT EXISTS(SELECT 1 FROM {source} WHERE coalesce(iw.status,'pending')!='ignored') THEN 'historical'"
        f" WHEN EXISTS(SELECT 1 FROM ({owners})) AND NOT EXISTS(SELECT 1 FROM {source} WHERE coalesce(iw.status,'pending') NOT IN ('ignored','deferred')) THEN 'deferred'"
    )


JOB_OWNERS = "SELECT item_id FROM main.item_requirements WHERE job_id=j.id AND state!='waived' UNION SELECT item_id FROM main.rule_requirements WHERE job_id=j.id AND state!='waived'"
PROCESSOR_OWNERS = "SELECT item_id FROM main.rule_requirements WHERE processor_job_id=j.id AND state!='waived'"
PROPOSAL_OWNERS = "SELECT item_id FROM main.item_requirements WHERE proposal_id=p.id AND state!='waived'"
EXTERNAL_SUCCESSOR = """EXISTS(SELECT 1 FROM main.rule_processor_jobs old
 JOIN main.rule_requirements replacement ON replacement.rule_id=old.rule_id
 AND replacement.file_id=old.file_id AND replacement.item_id=old.item_id
 JOIN catalog_workflow_checks c ON c.check_id=replacement.id AND c.profile=j.profile
 WHERE old.job_id=j.id AND replacement.profile=j.profile AND replacement.state!='waived'
 AND replacement.processor_job_id!=j.id AND c.satisfied=1)"""


CHECK_KEY = "'check:'||hex(c.item_id)||':'||hex(c.check_id)"
RELATED_CHECKS = """coalesce((SELECT json_group_array('check:'||hex(c.item_id)||':'||hex(c.check_id))
 FROM catalog_workflow_checks c WHERE c.item_id=w.item_id AND c.profile=w.profile AND coalesce(c.satisfied,0)=0),'[]')"""
JOB_CURRENT = "o.status='present' AND b.root IS NOT NULL AND " + _snapshot_matches(
    "j.snapshot", "o", "b"
)
SUCCESSOR = (
    """EXISTS(SELECT 1 FROM main.processing_jobs n
 WHERE n.id!=j.id AND n.profile=j.profile AND n.file_id=j.file_id AND n.operation=j.operation
 AND n.options=j.options AND n.recipe_id IS j.recipe_id AND n.state='complete'
 AND ((n.operation!='render' AND EXISTS(
 SELECT 1 FROM main.file_facts f WHERE f.profile=n.profile AND f.file_id=n.file_id
 AND f.operation=n.operation AND f.job_id=n.id AND f.status='complete' AND """
    + _snapshot_matches("f.snapshot", "o", "b")
    + ")) OR (n.operation='render' AND EXISTS(SELECT 1 FROM catalog_rendition_state rs WHERE rs.artifact_id=json_extract(n.result,'$.artifact_id') AND rs.recorded_current=1))))"
)

SQL = " UNION ALL ".join(
    [
        branch(
            """main.observation_jobs j WHERE j.generation=(SELECT max(n.generation) FROM main.observation_jobs n WHERE n.profile=j.profile AND n.source=j.source) AND (j.state IN ('failed','unavailable','stale') OR EXISTS(SELECT 1 FROM main.observation_scopes s WHERE s.job_id=j.id AND s.state IN ('failed','deferred')))""",
            work_key="'scan:'||hex(j.source)",
            profile="j.profile",
            evidence_profile="j.profile",
            subject_kind="'job'",
            subject_id="j.id",
            source_kind="'observation_jobs'",
            source_id="j.id",
            category="'observation'",
            label="j.source",
            reason="coalesce(json_extract(j.report,'$.blocker'),'unfinished_scan_coverage')",
            source_status="j.state",
            actionability="'blocked'",
            current="1",
            evidence_complete="0",
            evidence_at="j.completed_at",
            evidence="coalesce(j.report,'{}')",
            preconditions="json_object('job_id',j.id,'generation',j.generation,'compatibility',j.compatibility)",
            suggested_actions=action(
                "scan",
                "json_object('continue_request',(SELECT id FROM main.observation_requests r WHERE r.job_id=j.id AND r.state='active' ORDER BY created_at LIMIT 1))",
            ),
        ),
        branch(
            """catalog_files f WHERE NOT EXISTS
        (SELECT 1 FROM main.item_files a WHERE a.file_id=f.file_id AND a.active=1)""",
            work_key="'file:'||hex(f.file_id)",
            profile="f.profile",
            evidence_profile="f.profile",
            subject_kind="'file'",
            subject_id="f.file_id",
            source_kind="'files'",
            source_id="f.file_id",
            category="'identification'",
            label="f.source_relative_path",
            reason="'no_active_association'",
            source_status="f.status",
            actionability="CASE WHEN EXISTS(SELECT 1 FROM main.proposals p WHERE p.file_id=f.file_id AND p.profile=f.profile AND p.state='pending') THEN 'waiting' WHEN f.status='present' AND f.source_root IS NOT NULL THEN 'ready' ELSE 'blocked' END",
            countable="NOT EXISTS(SELECT 1 FROM main.proposals p WHERE p.file_id=f.file_id AND p.profile=f.profile AND p.state='pending')",
            related_work="(SELECT json_group_array('proposal:'||hex(p.id)) FROM main.proposals p WHERE p.file_id=f.file_id AND p.profile=f.profile AND p.state='pending')",
            evidence_at="f.observed_at",
            evidence_complete="(SELECT complete FROM main.scans WHERE id=f.scan_id)",
            evidence="json_object('availability',f.status,'scan_id',f.scan_id,'location',f.location)",
            preconditions="json_object('file_id',f.file_id,'size',CAST(f.size AS TEXT),'mtime_ns',CAST(f.mtime_ns AS TEXT),'device',CAST(f.device AS TEXT),'inode',CAST(f.inode AS TEXT),'root',f.source_root,'root_device',CAST(f.root_device AS TEXT),'root_inode',CAST(f.root_inode AS TEXT))",
            suggested_actions=action(
                "proposal put", "json_object('file_id',f.file_id)"
            ),
        ),
        branch(
            "catalog_item_workflow w JOIN main.items i ON i.id=w.item_id",
            work_key="'item:'||hex(w.item_id)",
            profile="w.profile",
            evidence_profile="w.profile",
            subject_kind="'item'",
            subject_id="w.item_id",
            source_kind="'item_workflow'",
            source_id="w.item_id",
            category="'curation'",
            label="coalesce(json_extract(i.metadata,'$.title'),w.item_id)",
            reason="CASE WHEN w.status='needs_attention' THEN 'completion_evidence_unsatisfied' ELSE 'curation_'||w.status END",
            source_status="w.requested_status",
            actionability="CASE WHEN w.status IN ('complete','ignored') THEN 'historical' WHEN w.status='deferred' THEN 'deferred' WHEN w.status='needs_attention' THEN 'blocked' ELSE 'ready' END",
            countable="NOT EXISTS(SELECT 1 FROM catalog_workflow_checks c WHERE c.item_id=w.item_id AND c.profile=w.profile AND coalesce(c.satisfied,0)=0)",
            related_work=RELATED_CHECKS,
            evidence_at="w.updated_at",
            evidence="json_object('effective_status',w.status,'metadata',json(i.metadata))",
            preconditions="json_object('workflow_revision',w.revision)",
            suggested_actions=action(
                "item status",
                "json_object('item_id',w.item_id,'expected_revision',w.revision)",
                note=True,
            ),
        ),
        branch(
            """catalog_workflow_checks c JOIN catalog_item_workflow w ON w.item_id=c.item_id AND w.profile=c.profile
        JOIN main.items ci ON ci.id=c.item_id
        LEFT JOIN main.item_requirements r ON r.id=c.check_id
        LEFT JOIN main.rule_requirements rr ON rr.id=c.check_id
        LEFT JOIN main.processing_jobs cj ON cj.id=coalesce(r.job_id,rr.job_id)
        LEFT JOIN main.processing_artifacts ca ON ca.id=r.artifact_id
        LEFT JOIN main.observations co ON co.profile=c.evidence_profile AND co.file_id=coalesce(r.file_id,cj.file_id,ca.file_id,CASE WHEN c.kind='file' THEN c.target_id END)
        LEFT JOIN main.scans cs ON cs.id=co.scan_id""",
            work_key=CHECK_KEY,
            profile="c.profile",
            evidence_profile="c.evidence_profile",
            subject_kind="'item'",
            subject_id="c.item_id",
            source_kind="CASE WHEN r.id IS NOT NULL THEN 'item_requirements' WHEN rr.id IS NOT NULL THEN 'rule_requirements' ELSE 'workflow_check' END",
            source_id="c.check_id",
            category="'requirement'",
            label="c.label",
            reason="CASE WHEN c.satisfied=1 THEN 'check_satisfied' ELSE 'required_'||c.kind||'_unsatisfied' END",
            source_status="coalesce(r.state,rr.state,c.state)",
            required="1",
            countable="NOT (r.id IS NULL AND rr.id IS NULL AND c.kind='file' AND EXISTS(SELECT 1 FROM main.item_requirements explicit WHERE explicit.item_id=c.item_id AND explicit.file_id=c.target_id AND explicit.profile=c.profile AND explicit.state!='waived'))",
            actionability="CASE WHEN c.satisfied=1 OR w.requested_status='ignored' THEN 'historical' WHEN w.requested_status='deferred' THEN 'deferred' WHEN c.kind='review' OR (c.kind='proposal' AND c.state='pending') THEN 'needs_decision' WHEN c.kind='metadata' THEN 'ready' WHEN c.state IN ('queued','running','submitted','leased') THEN 'waiting' ELSE 'blocked' END",
            related_work="CASE WHEN c.kind IN ('file','job','proposal') THEN json_array(c.kind||':'||hex(c.target_id)) WHEN rr.job_id IS NOT NULL THEN json_array('job:'||hex(rr.job_id)) WHEN rr.processor_job_id IS NOT NULL THEN json_array('processor:'||hex(rr.processor_job_id)) ELSE '[]' END",
            evidence_at="CASE WHEN c.kind='review' THEN w.updated_at ELSE coalesce(cj.finished_at,cs.finished_at) END",
            evidence_complete="cs.complete",
            evidence="json_object('kind',c.kind,'state',c.state,'satisfied',c.satisfied,'target_id',c.target_id,'evidence_profile',c.evidence_profile)",
            preconditions="json_object('workflow_revision',w.revision,'item_metadata',json(ci.metadata),'requirement_state',r.state,'job_id',cj.id,'job_state',cj.state,'job_attempts',cj.attempts,'job_snapshot',json(cj.snapshot),'artifact_state',ca.state,'publication_snapshot',json(ca.publication_snapshot),'processor_job_id',rr.processor_job_id,'file_id',co.file_id,'scan_id',co.scan_id,'size',CAST(co.size AS TEXT),'mtime_ns',CAST(co.mtime_ns AS TEXT),'device',CAST(co.device AS TEXT),'inode',CAST(co.inode AS TEXT))",
            suggested_actions="CASE WHEN c.kind='review' THEN "
            + action(
                "item resolve",
                "json_object('requirement_id',c.check_id,'expected_revision',w.revision)",
                note=True,
            )
            + " ELSE "
            + action("item requirements", "json_object('item_id',c.item_id)")
            + " END",
        ),
        branch(
            "main.proposals p",
            work_key="'proposal:'||hex(p.id)",
            profile="p.profile",
            evidence_profile="p.profile",
            subject_kind="'proposal'",
            subject_id="p.id",
            source_kind="'proposals'",
            source_id="p.id",
            category="'proposal'",
            label="p.kind||' proposal'",
            reason="'proposal_'||p.state",
            source_status="p.state",
            actionability="CASE WHEN p.state!='pending' THEN 'historical'"
            + owner_pause(PROPOSAL_OWNERS)
            + " ELSE 'needs_decision' END",
            required="EXISTS(SELECT 1 FROM main.item_requirements r WHERE r.proposal_id=p.id AND r.state!='waived')",
            countable="NOT EXISTS(SELECT 1 FROM main.item_requirements r WHERE r.proposal_id=p.id AND r.state!='waived')",
            related_work="json_array('file:'||hex(p.file_id))",
            evidence_at="p.created_at",
            preconditions="json_object('state',p.state,'snapshot',json(p.snapshot))",
            suggested_actions=action(
                "proposal show", "json_object('proposal_id',p.id)"
            ),
        ),
        branch(
            """main.processing_jobs j LEFT JOIN main.observations o ON o.file_id=j.file_id AND o.profile=j.profile
        LEFT JOIN main.bindings b ON b.profile=j.profile AND b.kind='source' AND b.owner=j.location""",
            work_key="'job:'||hex(j.id)",
            profile="j.profile",
            evidence_profile="j.profile",
            subject_kind="'job'",
            subject_id="j.id",
            source_kind="'processing_jobs'",
            source_id="j.id",
            category="'processing'",
            label="j.operation",
            reason="'processing_'||j.state",
            source_status="j.state",
            actionability="CASE WHEN j.state IN ('complete','cancelled') THEN 'historical' WHEN j.state IN ('queued','running') THEN 'waiting' WHEN "
            + SUCCESSOR
            + " THEN 'historical'"
            + owner_pause(JOB_OWNERS)
            + " ELSE 'blocked' END",
            required="EXISTS(SELECT 1 FROM main.item_requirements r WHERE r.job_id=j.id AND r.state!='waived') OR EXISTS(SELECT 1 FROM main.rule_requirements r WHERE r.job_id=j.id AND r.state!='waived')",
            countable="NOT EXISTS(SELECT 1 FROM main.item_requirements r WHERE r.job_id=j.id AND r.state!='waived') AND NOT EXISTS(SELECT 1 FROM main.rule_requirements r WHERE r.job_id=j.id AND r.state!='waived')",
            related_work="json_array('file:'||hex(j.file_id))",
            evidence_at="coalesce(j.finished_at,j.created_at)",
            current=JOB_CURRENT,
            evidence="json_object('error',j.error,'input_current',("
            + JOB_CURRENT
            + "))",
            preconditions="json_object('state',j.state,'attempts',j.attempts,'snapshot',json(j.snapshot),'cache_key',j.cache_key,'observed_size',CAST(o.size AS TEXT),'observed_mtime_ns',CAST(o.mtime_ns AS TEXT),'observed_device',CAST(o.device AS TEXT),'observed_inode',CAST(o.inode AS TEXT),'observed_root',b.root)",
            suggested_actions="CASE WHEN j.state IN ('failed','timeout') AND ("
            + JOB_CURRENT
            + ") THEN "
            + action("process retry", "json_object('job_id',j.id)")
            + " ELSE "
            + action("process show", "json_object('job_id',j.id)")
            + " END",
        ),
        branch(
            "main.processor_jobs j",
            work_key="'processor:'||hex(j.id)",
            profile="j.profile",
            evidence_profile="j.profile",
            subject_kind="'job'",
            subject_id="j.id",
            source_kind="'processor_jobs'",
            source_id="j.id",
            category="'processing'",
            label="'External processor '||j.processor",
            reason="'processor_'||j.state",
            source_status="j.state",
            actionability="CASE WHEN j.state IN ('complete','cancelled') THEN 'historical' WHEN j.state IN ('queued','running','submitted','pending','leased') THEN 'waiting'"
            + " WHEN "
            + EXTERNAL_SUCCESSOR
            + " THEN 'historical'"
            + owner_pause(PROCESSOR_OWNERS)
            + " ELSE 'blocked' END",
            required="EXISTS(SELECT 1 FROM main.rule_requirements r WHERE r.processor_job_id=j.id AND r.state!='waived')",
            countable="NOT EXISTS(SELECT 1 FROM main.rule_requirements r WHERE r.processor_job_id=j.id AND r.state!='waived')",
            evidence_at="j.updated_at",
            evidence="json_object('error',j.error)",
            preconditions="json_object('state',j.state,'generation',j.generation,'digest',j.digest)",
            suggested_actions=action("processor job", "json_object('job_id',j.id)"),
        ),
        branch(
            """main.processing_artifacts a JOIN main.processing_jobs j ON j.id=a.job_id
            LEFT JOIN main.observations o ON o.file_id=j.file_id AND o.profile=j.profile
            LEFT JOIN main.bindings b ON b.profile=j.profile AND b.kind='source' AND b.owner=j.location""",
            work_key="'artifact:'||hex(a.id)",
            profile="a.profile",
            evidence_profile="a.profile",
            subject_kind="'job'",
            subject_id="a.job_id",
            source_kind="'processing_artifacts'",
            source_id="a.id",
            category="'artifact_recovery'",
            label="a.path",
            reason="'artifact_'||a.state",
            source_status="a.state",
            actionability="CASE WHEN a.state='ready' OR EXISTS(SELECT 1 FROM main.processing_artifacts n WHERE n.job_id=a.job_id AND n.attempt>a.attempt AND n.state='ready') OR "
            + SUCCESSOR
            + " THEN 'historical' WHEN j.state='running' AND a.state IN ('planned','writing','validating','publishing') THEN 'waiting'"
            + owner_pause(
                JOB_OWNERS
                + " UNION SELECT item_id FROM main.item_requirements WHERE artifact_id=a.id AND state!='waived'"
            )
            + " ELSE 'blocked' END",
            countable="j.state NOT IN ('failed','timeout','changed','interrupted') AND NOT EXISTS(SELECT 1 FROM main.item_requirements r WHERE r.artifact_id=a.id AND r.state!='waived')",
            required="EXISTS(SELECT 1 FROM main.item_requirements r WHERE r.artifact_id=a.id AND r.state!='waived')",
            related_work="json_array('job:'||hex(a.job_id))",
            evidence_at="a.created_at",
            evidence="json_object('error',a.error,'job_state',j.state)",
            preconditions="json_object('state',a.state,'attempt',a.attempt,'binding',json(a.binding),'publication_snapshot',json(a.publication_snapshot))",
            suggested_actions="CASE WHEN a.state IN ('planned','writing','validating','publishing') THEN "
            + action("artifact recover")
            + " WHEN j.state IN ('failed','timeout','cancelled') AND ("
            + JOB_CURRENT
            + ") AND NOT EXISTS(SELECT 1 FROM main.processing_artifacts pending WHERE pending.job_id=j.id AND pending.state IN ('planned','writing','validating','publishing')) THEN "
            + action("process retry", "json_object('job_id',j.id)")
            + " ELSE "
            + action("artifact show", "json_object('id',a.id)")
            + " END",
        ),
        branch(
            """main.watchers w JOIN main.watcher_definitions d ON d.id=w.definition_id
        LEFT JOIN main.watcher_runs r ON r.id=(SELECT h.id FROM main.watcher_runs h WHERE h.profile=w.profile AND h.watcher=w.name AND h.definition_id=w.definition_id ORDER BY h.started_at DESC,h.id DESC LIMIT 1)
        LEFT JOIN main.watcher_runs pr ON pr.id=w.pending_reaction AND pr.profile=w.profile AND pr.watcher=w.name""",
            work_key="'watcher:'||hex(w.name)",
            profile="w.profile",
            evidence_profile="w.profile",
            subject_kind="'watcher'",
            subject_id="w.name",
            source_kind="'watchers'",
            source_id="w.name",
            category="CASE WHEN json_extract(d.definition,'$.reaction')='projection' THEN 'projection_reaction' ELSE 'watcher_reaction' END",
            label="w.name",
            reason="CASE WHEN pr.state IN ('failed','interrupted') THEN 'reaction_failed' WHEN r.error IS NOT NULL THEN 'watcher_failed' WHEN w.pending_reaction IS NOT NULL THEN 'reaction_pending' ELSE 'watcher_'||coalesce(r.state,'idle') END",
            source_status="coalesce(r.state,'idle')",
            actionability="CASE WHEN pr.state IN ('failed','interrupted') THEN 'blocked' WHEN w.active_run IS NOT NULL THEN 'waiting' WHEN r.state='waiting' THEN 'waiting' WHEN w.pending_reaction IS NOT NULL OR r.state IN ('failed','interrupted') THEN 'blocked' WHEN w.requested_generation>w.completed_generation OR (w.enabled=1 AND w.pending_generation>w.completed_generation) THEN 'waiting' ELSE 'historical' END",
            evidence_at="coalesce(r.completed_at,r.started_at)",
            evidence="json_object('error',coalesce(pr.error,r.error),'pending_reaction_state',pr.state,'enabled',w.enabled,'pending_reaction',w.pending_reaction,'run_id',r.id)",
            preconditions="json_object('definition_id',w.definition_id,'pending_generation',w.pending_generation,'completed_generation',w.completed_generation,'pending_reaction',w.pending_reaction,'active_run',w.active_run)",
            suggested_actions=action("watcher run", "json_object('name',w.name)"),
        ),
        branch(
            "main.fallback_entries e",
            work_key="'projection:'||hex(e.id)",
            profile="e.profile",
            evidence_profile="e.profile",
            subject_kind="'projection'",
            subject_id="e.catalog",
            source_kind="'fallback_entries'",
            source_id="e.id",
            category="'projection'",
            label="e.catalog",
            reason="'projection_'||e.state",
            source_status="e.state",
            actionability="CASE WHEN e.active=0 OR (e.state LIKE 'resolved%' AND e.published_generation=e.generation) THEN 'historical' WHEN e.state LIKE 'resolved%' THEN 'waiting' ELSE 'blocked' END",
            related_work="json_array('item:'||hex(e.item_id))",
            evidence="e.evidence",
            preconditions="json_object('generation',e.generation,'published_generation',e.published_generation,'policy_id',e.policy_id,'revision',e.revision)",
            suggested_actions=action(
                "fallback preview", "json_object('catalog',e.catalog)"
            ),
        ),
        branch(
            "main.journal j",
            work_key="'journal:'||hex(j.id)",
            profile="j.profile",
            evidence_profile="j.profile",
            subject_kind="'projection'",
            subject_id="j.catalog",
            source_kind="'journal'",
            source_id="j.id",
            category="'projection_recovery'",
            label="j.catalog",
            reason="'unfinished_link_journal'",
            source_status="j.kind",
            actionability="'blocked'",
            preconditions="json_object('journal_id',j.id,'path',j.path,'kind',j.kind,'target',j.target,'previous',j.previous)",
            suggested_actions=action("recover", "json_object('catalog',j.catalog)"),
        ),
        branch(
            "main.catalog_refresh_queue q",
            work_key="'refresh:'||hex(q.catalog)",
            profile="q.profile",
            evidence_profile="q.profile",
            subject_kind="'projection'",
            subject_id="q.catalog",
            source_kind="'catalog_refresh_queue'",
            source_id="q.catalog",
            category="'projection_refresh'",
            label="q.catalog",
            reason="CASE WHEN q.error IS NULL THEN 'refresh_pending' ELSE 'refresh_failed' END",
            source_status="CASE WHEN q.error IS NULL THEN 'pending' ELSE 'failed' END",
            actionability="CASE WHEN q.error IS NULL THEN 'waiting' ELSE 'blocked' END",
            evidence_at="q.updated_at",
            evidence="json_object('error',q.error)",
            preconditions="json_object('generation',q.generation,'attempts',q.attempts)",
            suggested_actions=action(
                "catalog-refresh run", "json_object('catalog',q.catalog)"
            ),
        ),
    ]
)


def install(db):
    """Connection-local views only; no durable cache or migration."""
    from .sql_query import VIEWS

    previous = db.execute("PRAGMA query_only").fetchone()[0]
    try:
        db.execute("PRAGMA query_only=OFF")
        for view, (_, sql) in VIEWS.items():
            db.execute(f"CREATE TEMP VIEW IF NOT EXISTS {view} AS {sql}")
    finally:
        db.execute(f"PRAGMA query_only={previous}")


def decode(row):
    for field in JSON_FIELDS:
        row[field] = json.loads(row[field])
    return row


def evidence_token(row):
    return hashlib.sha256(encode(row).encode()).hexdigest()


def validate_evidence(row, expected):
    """Read-side planning check, not an atomic mutation fence or authorization."""
    if evidence_token(row) != expected:
        raise CatabolicError(
            "inbox evidence changed; reload the owner record and replan"
        )
