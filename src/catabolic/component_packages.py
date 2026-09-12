# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Component package capture used inside the existing ordered fallback resolver."""

import hashlib
import json
from pathlib import PurePosixPath

from . import components
from .curation import occurrence
from .domain import CatabolicError
from .store import encode


class PackageBlocked(CatabolicError):
    pass


def compatible(component, video, primary):
    if (
        component["item_id"] != primary["item_id"]
        or component["part"] != primary["part"]
    ):
        return None
    if (
        component["file_id"] == video["file_id"]
        and component["revision"] == video["revision"]
    ):
        return {
            "basis": "container_timestamps",
            "edition_id": primary["metadata"].get("edition_id", primary["item_id"]),
            "video_file_id": video["file_id"],
            "video_revision": video["revision"],
            "part": primary["part"],
            "timeline_id": "container:" + video["revision"],
            "coverage": "container",
            "offset_seconds": 0.0,
            "synchronization": "container_timestamps",
        }
    claim = component["compatibility"]
    if not claim:
        return None
    timeline = primary["metadata"].get("timeline_id", "container:" + video["revision"])
    if (
        claim.get("edition_id")
        != primary["metadata"].get("edition_id", primary["item_id"])
        or claim.get("video_file_id") != video["file_id"]
        or claim.get("video_revision") != video["revision"]
        or claim.get("part") != primary["part"]
        or claim.get("timeline_id") != timeline
        or claim.get("coverage") != "full"
        or claim.get("synchronization") not in ("declared", "verified")
        or type(claim.get("offset_seconds")) not in (int, float)
    ):
        return None
    return {"basis": "accepted_compatibility", **claim}


def capture(resolver, primary, policy, check=None):
    store, profile, session = resolver.store, resolver.profile, resolver.session
    config = policy["package"]
    from .component_sql import OCCURRENCES_SQL

    videos = session.rows(
        "SELECT occurrence_id FROM ("
        + OCCURRENCES_SQL
        + ") WHERE profile=? AND association_id=? AND kind='video' AND current=1 ORDER BY occurrence_id LIMIT 257",
        (profile, primary["id"]),
    )
    if primary.get("video_occurrence_id"):
        videos = [
            r for r in videos if r["occurrence_id"] == primary["video_occurrence_id"]
        ]
    if not videos:
        return None
    if len(videos) != 1:
        raise PackageBlocked("ambiguous_video_component")
    video = components.get(
        store,
        profile,
        videos[0]["occurrence_id"],
        require_current=True,
        session=session,
    )
    if resolver.access and not resolver.access.component(video["occurrence_id"]):
        return None
    selected = [
        {
            "requirement": "video",
            "occurrence": video,
            "compatibility": compatible(video, video, primary),
        }
    ]
    # Publication revalidates the admitted occurrence in its saved query tiers;
    # the journal performs the final live group check before touching outputs.
    pinned = primary.pop("component_choices", None)
    total = 0
    query_reports = []

    def supporting(value):
        dependencies = []
        for dep in value["dependencies"]:
            snap = occurrence(store, profile, dep["file_id"])
            if (
                components.revision(snap) != dep["revision"]
                or snap["status"] != "present"
            ):
                return None
            if resolver.access:
                from .content_access import authorize

                authorize(resolver.access, dep["file_id"], dep["revision"])
            dependencies.append(
                {**dep, "snapshot": snap, "for_occurrence_id": value["occurrence_id"]}
            )
        return dependencies

    for requirement in config["requirements"]:
        chosen = None
        for tier in requirement["fallbacks"]:
            entity, ids, report = session.select(tier["query_id"])
            if not report["complete"] or entity not in (
                "component_id",
                "occurrence_id",
            ):
                raise PackageBlocked(
                    "component_requirement_needs_complete_component_selection"
                )
            rows = session.rows(
                "SELECT occurrence_id FROM ("
                + OCCURRENCES_SQL
                + ") WHERE profile=? AND item_id=? AND "
                + entity
                + " IN (SELECT value FROM json_each(?)) AND current=1 ORDER BY occurrence_id LIMIT ?",
                (
                    profile,
                    primary["item_id"],
                    encode(sorted(ids)),
                    policy["budgets"]["max_candidates"] + 1,
                ),
            )
            total += len(rows)
            if total > policy["budgets"]["max_candidates"]:
                raise PackageBlocked("component_candidate_budget")
            usable = []
            for row in rows:
                if resolver.access and not resolver.access.component(
                    row["occurrence_id"]
                ):
                    continue
                value = components.get(
                    store, profile, row["occurrence_id"], session=session
                )
                if not value["current"] or value["conflicts"]:
                    continue
                relation = compatible(value, video, primary)
                if relation:
                    usable.append(
                        {
                            "requirement": requirement["name"],
                            "occurrence": value,
                            "compatibility": relation,
                        }
                    )
            query_reports.append(
                {
                    "requirement": requirement["name"],
                    "query_id": tier["query_id"],
                    "eligible": len(usable),
                }
            )
            if len({r["occurrence"]["component_id"] for r in usable}) > 1:
                raise PackageBlocked(
                    "ambiguous_component_requirement:" + requirement["name"]
                )
            for candidate in usable:
                value = candidate["occurrence"]
                if (
                    pinned is not None
                    and pinned.get(requirement["name"]) != value["occurrence_id"]
                ):
                    continue
                dependencies = supporting(value)
                if dependencies is None:
                    continue
                snapshots = [value["snapshot"]] + [d["snapshot"] for d in dependencies]
                if len(snapshots) > policy["budgets"]["max_checks"]:
                    raise PackageBlocked("component_dependency_budget")
                if check is not None and not check(
                    snapshots, value["snapshot"]["location"]
                ):
                    continue
                chosen = candidate
                break
            if chosen:
                break
        if chosen:
            selected.append(chosen)
        elif requirement["required"]:
            return None
    checks = list(primary["checks"])
    dependencies = []
    for entry in selected:
        value = entry["occurrence"]
        checks.append(value["snapshot"])
        supporting_assets = supporting(value)
        if supporting_assets is None:
            return None
        checks.extend(d["snapshot"] for d in supporting_assets)
        dependencies.extend(supporting_assets)
    checks = list({encode(c): c for c in checks}.values())
    if len(checks) > policy["budgets"]["max_checks"]:
        raise PackageBlocked("component_dependency_budget")
    packaging = decision(config, selected, dependencies)
    package = {
        "version": 1,
        "video": {
            "occurrence_id": video["occurrence_id"],
            "file_id": video["file_id"],
            "revision": video["revision"],
        },
        "components": selected,
        "dependencies": dependencies,
        "packaging": packaging,
        "queries": query_reports,
    }
    # Only semantic input evidence enters the identity; no clock or live-check count.
    package["signature"] = hashlib.sha256(
        encode({k: v for k, v in package.items() if k != "queries"}).encode()
    ).hexdigest()
    session.account(package)
    if packaging["action"] == "block":
        raise PackageBlocked(packaging["reason"])
    primary["checks"] = checks
    primary["evidence"]["component_package"] = package
    if not packaging["ready"]:
        from .component_packaging import resolved_artifact

        return resolved_artifact(resolver, primary, package)
    return primary


def decision(config, selected, dependencies):
    occurrences = [v["occurrence"] for v in selected]
    video = occurrences[0]
    external = [v for v in occurrences if v["file_id"] != video["file_id"]]
    offsets = any(v["compatibility"]["offset_seconds"] != 0 for v in selected)
    if config.get("supported_codecs") is not None and any(
        v["codec"] not in config["supported_codecs"] for v in occurrences
    ):
        return {
            "action": "convert",
            "ready": False,
            "operation_id": config.get("operation_id"),
            "reason": "approved_conversion_required",
            "exposes_unselected_components": False,
        }
    exact_container = (
        not external
        and not dependencies
        and not offsets
        and video["stream_count"] is not None
        and len({v["locator"].get("index") for v in occurrences})
        == video["stream_count"]
    )
    if config["publication"] == "selected_only" and exact_container:
        return {
            "action": "use_container",
            "ready": True,
            "exposes_unselected_components": False,
        }
    embedded_dependencies = any(
        d["for_occurrence_id"]
        in {v["occurrence_id"] for v in occurrences if v["file_id"] == video["file_id"]}
        for d in dependencies
    )
    if config["publication"] == "selected_only" or offsets or embedded_dependencies:
        action = "mux"
    elif external:
        action = "publish_sidecars"
        if config["publication"] == "container":
            action = "mux"
    else:
        action = "use_container"
    if action == "publish_sidecars":
        if any(
            v["locator"]["convention"] != "whole_file"
            and not (
                v["stream_count"] == 1
                and v["format_name"]
                in ("srt", "webvtt", "ass", "flac", "aac", "mp3", "wav", "ogg")
            )
            for v in external
        ):
            return {
                "action": "extract",
                "ready": False,
                "operation_id": config.get("operation_id"),
                "reason": "external_component_needs_extracted_artifact",
                "exposes_unselected_components": True,
            }
        if any(d["purpose"] == "font" for d in dependencies):
            return {
                "action": "block",
                "ready": False,
                "reason": "font_sidecar_layout_unsupported",
                "exposes_unselected_components": True,
            }
    if action == "mux" and any(
        d["purpose"] in ("subtitle_index", "subtitle_data") for d in dependencies
    ):
        return {
            "action": "block",
            "ready": False,
            "reason": "paired_subtitle_mux_unsupported",
            "exposes_unselected_components": False,
        }
    for v in occurrences:
        extension = PurePosixPath(v["snapshot"]["path"]).suffix.lower()
        if (
            v["codec"] in ("ass", "ssa", "dvd_subtitle")
            or extension in (".ass", ".ssa", ".idx", ".sub")
        ) and action not in ("use_container",):
            if not v["dependencies_complete"]:
                return {
                    "action": "block",
                    "ready": False,
                    "reason": "subtitle_dependencies_unverified",
                    "exposes_unselected_components": False,
                }
    return {
        "action": action,
        "ready": action in ("use_container", "publish_sidecars"),
        "operation_id": config.get("operation_id"),
        "exposes_unselected_components": action
        in ("use_container", "publish_sidecars"),
    }


def projection_members(store, profile, result):
    """Expand a package into the existing single publication plan and journal."""
    from uuid import UUID, uuid5

    from .content_access import revision_of

    additional = []
    for decision in list(result["decisions"]):
        package = decision["evidence"].get("component_package")
        if (
            not package
            or not package["packaging"]["ready"]
            or package["packaging"]["action"] != "publish_sidecars"
        ):
            continue
        for entry in package["components"][1:]:
            value = entry["occurrence"]
            if value["file_id"] == package["video"]["file_id"]:
                continue
            association = store.rows(
                "SELECT * FROM item_files WHERE id=?", (value["association_id"],)
            )[0]
            metadata = json.loads(association["metadata"])
            metadata.update(
                {k: value[k] for k in ("language", "forced") if value[k] is not None}
            )
            assets = [("component", value["file_id"])] + [
                (d["purpose"], d["file_id"])
                for d in package["dependencies"]
                if d["for_occurrence_id"] == value["occurrence_id"]
            ]
            for purpose, file_id in assets:
                file = store.rows("SELECT * FROM files WHERE id=?", (file_id,))[0]
                key = str(
                    uuid5(
                        UUID(decision["entry_id"]),
                        encode([entry["requirement"], purpose]),
                    )
                )
                if key in result["captures"]:
                    raise PackageBlocked("ambiguous_component_supporting_asset")
                capture = {
                    **association,
                    "id": key,
                    "file_id": file_id,
                    "path": file["path"],
                    "location": file["location"],
                    "metadata": metadata,
                    "revision": revision_of(store, profile, file_id),
                    "checks": [occurrence(store, profile, file_id)],
                    "evidence": {},
                    "rendition_id": None,
                }
                child = {
                    **decision,
                    "entry_id": key,
                    "role": association["role"],
                    "file_id": file_id,
                    "revision": capture["revision"],
                    "rendition_id": None,
                    "content_path": f"/v1/files/{file_id}/content?revision={capture['revision']}",
                    "evidence": {
                        "component_package_signature": package["signature"],
                        "occurrence_id": value["occurrence_id"],
                        "parent_entry_id": decision["entry_id"],
                    },
                }
                result["captures"][key] = capture
                additional.append(child)
    result["decisions"].extend(additional)


def publication_capture(reconciler, mapping, policy, entity, ids):
    """Revalidate the admitted group using the same resolver before journal I/O."""
    from .evaluation import EvaluationSession
    from .fallback_resolution import Resolver

    store, profile = reconciler.store, reconciler.profile
    mapping = store.rows(
        "SELECT * FROM fallback_entries WHERE id=? AND profile=?",
        (mapping["id"], profile),
    )[0]
    evidence = json.loads(mapping["evidence"])
    parent = mapping
    if evidence.get("parent_entry_id"):
        rows = store.rows(
            "SELECT * FROM fallback_entries WHERE profile=? AND catalog=? AND id=? AND active=1",
            (profile, mapping["catalog"], evidence["parent_entry_id"]),
        )
        if not rows:
            return None
        parent = rows[0]
    package = json.loads(parent["evidence"]).get("component_package")
    if not package or not package["packaging"]["ready"]:
        return None
    video = components.get(
        store, profile, package["video"]["occurrence_id"], require_current=True
    )
    eligible = {
        "file_id": video["file_id"],
        "item_id": video["item_id"],
        "association_id": video["association_id"],
        "component_id": video["component_id"],
        "occurrence_id": video["occurrence_id"],
    }
    if eligible.get(entity) not in ids:
        return None
    rows = store.rows(
        "SELECT a.*,f.location,f.path FROM item_files a JOIN files f ON f.id=a.file_id WHERE a.id=?",
        (video["association_id"],),
    )
    if not rows:
        return None
    limits = policy["budgets"]
    session = EvaluationSession(
        store,
        profile,
        timeout_ms=limits["query_timeout_ms"],
        max_ids=limits["max_candidates"],
    )
    accepted = Resolver(reconciler.app, session=session).capture(
        {
            **rows[0],
            "video_occurrence_id": video["occurrence_id"],
            "component_choices": {
                entry["requirement"]: entry["occurrence"]["occurrence_id"]
                for entry in package["components"]
            },
        },
        policy,
        mapping["catalog"],
    )
    if not accepted:
        return None
    fresh = accepted["evidence"]["component_package"]
    if (
        fresh["signature"] != package["signature"]
        or fresh["packaging"] != package["packaging"]
        or accepted["file_id"] != parent["file_id"]
    ):
        return None
    if parent is not mapping:
        result = {
            "decisions": [
                {"entry_id": parent["id"], "evidence": {"component_package": fresh}}
            ],
            "captures": {},
        }
        projection_members(store, profile, result)
        child = result["captures"].get(mapping["id"])
        if (
            not child
            or child["file_id"] != mapping["file_id"]
            or child["revision"] != mapping["revision"]
        ):
            return None
    return accepted
