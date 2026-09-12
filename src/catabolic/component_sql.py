# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Shared SQL component surface, backed by retained evidence rather than copies."""

import json

from .revision_evidence import _snapshot_matches

LANGUAGES = dict(
    zip(
        (
            "eng",
            "fra",
            "fre",
            "deu",
            "ger",
            "spa",
            "jpn",
            "zho",
            "chi",
            "ita",
            "por",
            "rus",
            "kor",
            "hin",
            "ara",
        ),
        (
            "en",
            "fr",
            "fr",
            "de",
            "de",
            "es",
            "ja",
            "zh",
            "zh",
            "it",
            "pt",
            "ru",
            "ko",
            "hi",
            "ar",
        ),
        strict=True,
    )
)


def language(value):
    if (
        not isinstance(value, str)
        or not value.strip()
        or value.lower() in ("und", "unknown")
    ):
        return None
    return LANGUAGES.get(value.lower(), value.lower())


def normalize(raw, metadata, assertions, format_name, inherited):
    stream, metadata, assertions = (
        json.loads(raw or "{}"),
        json.loads(metadata or "{}"),
        json.loads(assertions or "[]"),
    )
    tags, flags = stream.get("tags", {}), stream.get("disposition", {})
    # Raw subtitle demuxers synthesize zero dispositions; these formats cannot
    # encode a playback default or forced designation. Retain zeros in technical.
    if format_name in ("srt", "webvtt", "ass", "ssa", "microdvd", "subviewer"):
        flags = {}
    observed = {"language": language(tags.get("language")), "title": tags.get("title")}
    for key, flag in [
        ("default", "default"),
        ("forced", "forced"),
        ("commentary", "comment"),
        ("hearing_impaired", "hearing_impaired"),
        ("visual_impaired", "visual_impaired"),
    ]:
        value = flags.get(flag)
        observed[key] = bool(value) if type(value) is int and value in (0, 1) else None
    inherited = json.loads(inherited or "{}")
    asserted = {k: v for k, v in inherited.items() if k in observed and v is not None}
    asserted.update(
        {
            k: metadata[k]
            for k in (
                "language",
                "title",
                "forced",
                "default",
                "commentary",
                "hearing_impaired",
                "visual_impaired",
            )
            if k in metadata
        }
    )
    if "sdh" in metadata:
        asserted["hearing_impaired"] = metadata["sdh"]
    if "language" in asserted:
        asserted["language"] = language(asserted["language"])
    conflicts, compat, deps, identities, complete = [], [], [], [], []
    for assertion in assertions:
        identities.append(assertion["component_id"])
        payload = assertion["payload"]
        if payload.get("compatibility") is not None:
            compat.append(payload["compatibility"])
        deps.extend(payload.get("dependencies", []))
        if payload.get("dependencies_complete") is not None:
            complete.append(payload["dependencies_complete"])
        for key, value in payload.get("attributes", {}).items():
            value = language(value) if key == "language" else value
            if key in asserted and asserted[key] != value:
                conflicts.append(key)
            asserted[key] = value
    if len(set(identities)) > 1:
        conflicts.append("component_identity")
    if len({json.dumps(c, sort_keys=True) for c in compat}) > 1:
        conflicts.append("compatibility")
    effective = {}
    for key, value in observed.items():
        claim = asserted.get(key)
        if value is not None and claim is not None and value != claim:
            conflicts.append(key)
        effective[key] = (
            None if key in conflicts else value if value is not None else claim
        )
    return json.dumps(
        {
            "observed": observed,
            "asserted": asserted,
            "effective": effective,
            "compatibility": compat[0]
            if compat and "compatibility" not in conflicts
            else None,
            "dependencies": list(
                {json.dumps(d, sort_keys=True): d for d in deps}.values()
            ),
            "dependencies_complete": complete[0]
            if complete and len(set(complete)) == 1
            else None,
            "conflicts": sorted(set(conflicts)),
        },
        sort_keys=True,
    )


def install(db):
    db.create_function("component_normalize", 5, normalize, deterministic=True)


OCCURRENCES_SQL = (
    """WITH evidence AS (
 SELECT c.*, m.kind, a.role AS association_role,
 CASE WHEN c.probe_path='' THEN j.result ELSE json_extract(j.result,'$.' || c.probe_path) END AS retained_container,
 (SELECT json_extract(ji.evidence,'$.occurrence') FROM main.component_output_lineage ln JOIN main.processing_artifacts ar ON ar.id=ln.artifact_id JOIN main.component_job_inputs ji ON ji.job_id=ar.job_id AND ji.ordinal=ln.ordinal WHERE ar.file_id=c.file_id AND ar.profile=c.profile AND ar.job_id=c.probe_job_id AND ln.output_stream_index=json_extract(c.locator,'$.index') LIMIT 1) AS inherited,
 json_extract(j.result,'$.' || CASE WHEN c.probe_path='' THEN '' ELSE c.probe_path || '.' END || 'streams[' || c.stream_key || ']') AS technical,
 (SELECT json_group_array(json_object('id',x.id,'component_id',x.component_id,'payload',json(x.payload)))
 FROM main.component_assertions x WHERE x.occurrence_id=c.id AND x.active=1) AS assertions,
 coalesce((SELECT x.component_id FROM main.component_assertions x WHERE x.occurrence_id=c.id AND x.active=1 ORDER BY x.id LIMIT 1),(SELECT l.component_id FROM main.component_output_lineage l JOIN main.processing_artifacts ar ON ar.id=l.artifact_id WHERE ar.file_id=c.file_id AND ar.profile=c.profile AND ar.job_id=c.probe_job_id AND l.output_stream_index=json_extract(c.locator,'$.index') LIMIT 1),c.component_id) AS logical_id,
 coalesce(o.status='present' AND (a.active=1 OR EXISTS (SELECT 1 FROM main.media_outputs mo WHERE mo.file_id=a.file_id AND mo.item_id=a.item_id AND mo.profile=c.profile)) AND a.metadata=json_extract(c.association_evidence,'$.metadata')
 AND a.part IS json_extract(c.association_evidence,'$.part') AND
 """
    + _snapshot_matches("c.snapshot", "o", "b")
    + """
 AND coalesce(json_extract(c.snapshot,'$.source_policy.revision'),0)=coalesce(p.revision,0)
 AND json_extract(c.snapshot,'$.volume_uuid') IS v.volume_uuid
 AND (c.probe_job_id IS NULL OR EXISTS (SELECT 1 FROM main.file_facts ff, json_each(ff.data,'$.streams') stream WHERE ff.profile=c.profile AND ff.file_id=c.file_id AND ff.operation='probe' AND ff.status='complete' AND json_extract(stream.value,'$.index')=json_extract(c.locator,'$.index') AND stream.value IS json_extract(j.result,'$.' || CASE WHEN c.probe_path='' THEN '' ELSE c.probe_path || '.' END || 'streams[' || c.stream_key || ']')))
 AND (c.probe_job_id IS NOT NULL OR NOT EXISTS (SELECT 1 FROM main.component_occurrences newer
 WHERE newer.association_id=c.association_id AND newer.profile=c.profile AND newer.revision=c.revision AND newer.probe_job_id IS NOT NULL)),0) AS current
 FROM main.component_occurrences c JOIN main.media_components m ON m.id=c.component_id
 JOIN main.item_files a ON a.id=c.association_id JOIN main.items i ON i.id=c.item_id
 JOIN main.files f ON f.id=c.file_id
 LEFT JOIN main.processing_jobs j ON j.id=c.probe_job_id
 LEFT JOIN main.observations o ON o.file_id=c.file_id AND o.profile=c.profile
 LEFT JOIN main.bindings b ON b.profile=c.profile AND b.kind='source' AND b.owner=f.location
 LEFT JOIN main.source_identity_policies p ON p.profile=c.profile AND p.location=f.location
 LEFT JOIN main.binding_volumes v ON v.profile=c.profile AND v.kind='source' AND v.owner=f.location
), containers AS (
 SELECT *,CASE WHEN current=1 AND probe_job_id IS NOT NULL THEN
 (SELECT ff.data FROM main.file_facts ff WHERE ff.profile=evidence.profile AND ff.file_id=evidence.file_id AND ff.operation='probe' AND ff.status='complete')
 ELSE retained_container END AS container FROM evidence
), normalized AS (
 SELECT *,json_extract(container,'$.format.format_name') AS format_name,
 json_array_length(json_extract(container,'$.streams')) AS stream_count,
 component_normalize(technical,json_extract(association_evidence,'$.metadata'),assertions,json_extract(container,'$.format.format_name'),inherited) AS n FROM containers
)
SELECT id AS occurrence_id,logical_id AS component_id,profile,item_id,file_id,association_id,kind,
 CASE WHEN json_extract(n,'$.effective.commentary')=1 THEN 'commentary' ELSE kind END AS role,
 json_extract(n,'$.effective.language') AS language,json_extract(n,'$.effective.title') AS title,
 json_extract(technical,'$.codec_name') AS codec,
 json_extract(n,'$.effective.default') AS default_flag,json_extract(n,'$.effective.forced') AS forced,
 json_extract(n,'$.effective.commentary') AS commentary,
 json_extract(n,'$.effective.hearing_impaired') AS hearing_impaired,
 json_extract(n,'$.effective.visual_impaired') AS visual_impaired,
 storage,locator,revision,snapshot,current,format_name,stream_count,
 (probe_job_id IS NOT NULL AND current=1) AS technically_verified,
 coalesce(technical,'{}') AS technical,
 json_extract(n,'$.observed') AS observed,json_extract(n,'$.asserted') AS asserted,
 json_extract(n,'$.compatibility') AS compatibility,json_extract(n,'$.dependencies') AS dependencies,
 json_extract(n,'$.dependencies_complete') AS dependencies_complete,
 json_extract(n,'$.conflicts') AS conflicts,
 json_extract(association_evidence,'$.part') AS part,
 json_object('probe_job_id',probe_job_id,'association_id',association_id,'assertions',json(assertions)) AS provenance
FROM normalized"""
)

COMPONENTS_SQL = (
    """SELECT component_id,kind,profile,json_group_array(DISTINCT item_id) AS item_ids FROM ("""
    + OCCURRENCES_SQL
    + ") GROUP BY component_id,kind,profile"
)

VIEWS = {
    "catalog_component_occurrences": (
        "Revision-pinned embedded and external components; NULL attributes remain unknown. Technical evidence and accepted assertions are separate.",
        OCCURRENCES_SQL,
    ),
    "catalog_components": (
        "Logical identities; only explicit accepted evidence joins occurrences.",
        COMPONENTS_SQL,
    ),
    "catalog_component_lineage": (
        "Verified generated-output component mappings and retained input occurrences.",
        "SELECT * FROM main.component_output_lineage",
    ),
}
