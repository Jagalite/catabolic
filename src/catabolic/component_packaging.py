# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Explicit component packaging through the existing artifact execution owner."""

import copy
import hashlib
import json
import os
import time
from contextlib import ExitStack

from . import components
from .curation import occurrence
from .domain import CatabolicError
from .source_access import validated_source
from .store import encode


def packet_evidence(fd, index, identity, poll, timeout):
    """Strict bounded copy verification; excessive evidence fails the job closed."""
    from .process_runner import command_output
    from .rendering import FORMATS

    os.lseek(fd, 0, os.SEEK_SET)
    data = command_output(
        [
            identity["ffprobe"]["tool"],
            "-v",
            "error",
            "-protocol_whitelist",
            "file,pipe",
            "-format_whitelist",
            FORMATS,
            "-max_alloc",
            "33554432",
            "-select_streams",
            str(index),
            "-show_packets",
            "-show_entries",
            "packet=pts_time,dts_time,duration_time,data_hash",
            "-show_data_hash",
            "sha256",
            "-of",
            "json",
            f"/dev/fd/{fd}",
        ],
        pass_fds=(fd,),
        timeout=timeout,
        maximum=64 * 1024 * 1024,
        on_poll=poll,
    )
    packets = json.loads(data).get("packets", [])
    if not packets or any(not p.get("data_hash") for p in packets):
        raise CatabolicError("component packet evidence is incomplete")
    return packets


def verify_packets(source, target, offset):
    from decimal import Decimal

    if len(source) != len(target):
        raise CatabolicError("mux changed the component packet count")
    for before, after in zip(source, target, strict=True):
        if before["data_hash"] != after["data_hash"]:
            raise CatabolicError("mux changed a selected component payload")
        for key in ("pts_time", "dts_time", "duration_time"):
            a, b = before.get(key), after.get(key)
            if a is None or b is None:
                if a != b:
                    raise CatabolicError("mux timing verification is incomplete")
            elif abs(
                Decimal(b)
                - Decimal(a)
                - (Decimal(str(offset)) if key != "duration_time" else 0)
            ) > Decimal("0.002"):
                raise CatabolicError(
                    "mux changed component timing beyond the accepted offset"
                )
    return {
        "packet_count": len(source),
        "payload_sha256": hashlib.sha256(
            encode([p["data_hash"] for p in source]).encode()
        ).hexdigest(),
        "input_timing_sha256": hashlib.sha256(encode(source).encode()).hexdigest(),
        "output_timing_sha256": hashlib.sha256(encode(target).encode()).hexdigest(),
        "accepted_offset_seconds": offset,
        "maximum_timestamp_rounding_seconds": 0.002,
    }


def validate(store, profile, package, recipe=None, *, live=False):
    if not package.get("components") or len(package["components"]) > 17:
        raise CatabolicError("invalid component package")
    for entry in package["components"]:
        old = entry["occurrence"]
        row = components.get(store, profile, old["occurrence_id"], require_current=True)
        if encode(row) != encode(old):
            raise CatabolicError("component evidence changed since packaging admission")
    for dep in package["dependencies"]:
        snap = occurrence(store, profile, dep["file_id"])
        if components.revision(snap) != dep["revision"]:
            raise CatabolicError(
                "component dependency changed since packaging admission"
            )
    if live:
        snapshots = [e["occurrence"]["snapshot"] for e in package["components"]] + [
            d["snapshot"] for d in package["dependencies"]
        ]
        for snapshot in {encode(s): s for s in snapshots}.values():
            with validated_source(snapshot):
                pass
    if recipe is not None:
        if package["packaging"].get("operation_id") != recipe["id"]:
            raise CatabolicError("package requires its explicitly approved operation")
        if (
            package["packaging"]["action"] == "mux"
            and recipe["preset"] != "component-mux"
        ):
            raise CatabolicError(
                "multi-input packaging requires a component-mux operation"
            )
        if package["packaging"]["action"] not in ("mux", "extract", "convert"):
            raise CatabolicError("package does not require processing")
        if package["packaging"]["action"] in ("extract", "convert"):
            if len(package["components"]) != 1 or recipe["preset"] not in (
                "subtitle-srt",
                "audio-flac",
                "audio-aac",
                "audio-opus",
                "audio-normalize",
                "remux-mkv",
            ):
                raise CatabolicError(
                    "multi-component conversion is not supported by this operation"
                )
            from .rendering import PRESETS

            kind = PRESETS[recipe["preset"]][3]
            if (
                kind is not None
                and package["components"][0]["occurrence"]["kind"] != kind
            ):
                raise CatabolicError(
                    "approved extraction operation has a different component kind"
                )


def enqueue(app, policy_id, item_id, location, expected_plan):
    from .artifacts import Artifacts
    from .fallback_resolution import Resolver

    result = Resolver(app).resolve([{"item_id": item_id}], policy_id)
    if result["plan_id"] != expected_plan:
        raise CatabolicError("stale_or_missing_component_package_plan")
    decision = result["decisions"][0]
    if not decision["state"].startswith("resolved"):
        raise CatabolicError("component package is unresolved")
    package = decision["evidence"].get("component_package")
    if not package:
        raise CatabolicError("policy does not select a component package")
    if not package["packaging"].get("operation_id"):
        raise CatabolicError("component package has no approved operation")
    return Artifacts(app).enqueue(
        package["video"]["file_id"],
        package["packaging"]["operation_id"],
        location,
        item_id,
        _component_package=package,
    )


def render_mux(output_fd, recipe, identity, poll, package):
    """No catalog writes here: the artifact owner holds the job claim and journal."""
    from . import rendering
    from .process_runner import command_output

    started = time.monotonic()
    outer_poll = poll

    def poll():
        outer_poll()
        if time.monotonic() - started > recipe["timeout"]:
            raise CatabolicError("component mux exceeded operation time budget")

    command = [
        identity["ffmpeg"]["tool"],
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-y",
        "-copyts",
    ]
    sources, selected, handles, attachments = [], [], [], []
    with ExitStack() as stack:
        for entry in package["components"]:
            row = entry["occurrence"]
            if row["locator"]["convention"] != "ffprobe_absolute_stream_index":
                raise CatabolicError(
                    "component mux requires an explicitly probed stream locator"
                )
            fd = stack.enter_context(validated_source(row["snapshot"]))
            handles.append(fd)
            before = rendering.probe(fd, identity)
            matches = [
                s
                for s in before["streams"]
                if s.get("index") == row["locator"]["index"]
            ]
            if (
                len(matches) != 1
                or matches[0].get("codec_type") != row["kind"]
                or matches[0].get("codec_name") != row["codec"]
            ):
                raise CatabolicError(
                    "pinned component stream does not match its probe evidence"
                )
            stream = copy.deepcopy(matches[0])
            for key in ("language", "title"):
                if row[key] is not None and not stream.get("tags", {}).get(key):
                    stream.setdefault("tags", {})[key] = row[key]
            for key, flag in (
                ("default_flag", "default"),
                ("forced", "forced"),
                ("commentary", "comment"),
                ("hearing_impaired", "hearing_impaired"),
                ("visual_impaired", "visual_impaired"),
            ):
                if row[key] is not None:
                    stream.setdefault("disposition", {})[flag] = int(row[key])
            os.lseek(fd, 0, os.SEEK_SET)
            command += [
                "-protocol_whitelist",
                "file,pipe",
                "-format_whitelist",
                rendering.FORMATS,
                "-threads",
                "1",
                "-itsoffset",
                str(entry["compatibility"]["offset_seconds"]),
                "-i",
                f"/dev/fd/{fd}",
            ]
            sources.append((len(sources), stream["index"]))
            selected.append(stream)
        for i, (source, index) in enumerate(sources):
            command += ["-map", f"{source}:{index}"]
            # Preserve observed stream flags. Unknown source flags remain unknown
            # in the plan; output flags are verified as new packaging evidence.
            flags = selected[i].get("disposition", {})
            supported = (
                "default",
                "forced",
                "dub",
                "original",
                "comment",
                "lyrics",
                "karaoke",
                "hearing_impaired",
                "visual_impaired",
                "clean_effects",
                "attached_pic",
            )
            command += [
                "-disposition:" + str(i),
                "+".join(k for k in supported if flags.get(k) == 1) or "0",
            ]
        command += ["-map_metadata", "-1", "-map_chapters", "-1", "-c", "copy"]
        for i, stream in enumerate(selected):
            for key in ("language", "title"):
                if stream.get("tags", {}).get(key) is not None:
                    command += [f"-metadata:s:{i}", key + "=" + stream["tags"][key]]
        for dep in package["dependencies"]:
            if dep["purpose"] != "font":
                raise CatabolicError("component mux supports font dependencies only")
            fd = stack.enter_context(validated_source(dep["snapshot"]))
            handles.append(fd)
            filename = os.path.basename(dep["snapshot"]["path"])
            extension = os.path.splitext(filename)[1].lower()
            if extension not in (".ttf", ".otf"):
                raise CatabolicError("unsupported font dependency format")
            n = len(attachments)
            command += [
                "-attach",
                f"/dev/fd/{fd}",
                f"-metadata:s:t:{n}",
                "mimetype="
                + (
                    "application/x-truetype-font"
                    if extension == ".ttf"
                    else "application/vnd.ms-opentype"
                ),
                f"-metadata:s:t:{n}",
                "filename=" + filename,
            ]
            attachments.append(
                {
                    "file_id": dep["file_id"],
                    "revision": dep["revision"],
                    "sha256": rendering.digest(fd),
                }
            )
            os.lseek(fd, 0, os.SEEK_SET)
        command += [
            "-max_alloc",
            "33554432",
            "-threads",
            "1",
            "-fs",
            str(recipe["max_output_bytes"]),
            "-f",
            "matroska",
            f"/dev/fd/{output_fd}",
        ]
        command_output(
            command,
            pass_fds=(*handles, output_fd),
            timeout=recipe["timeout"],
            on_poll=poll,
        )
        poll()
        os.fsync(output_fd)
        after = rendering.probe(output_fd, identity)
        streams = [s for s in after["streams"] if s.get("codec_type") != "attachment"]
        if [(s.get("codec_type"), s.get("codec_name")) for s in streams] != [
            (s.get("codec_type"), s.get("codec_name")) for s in selected
        ]:
            raise CatabolicError(
                "mux output does not contain exactly the selected component representations"
            )
        if len(
            [s for s in after["streams"] if s.get("codec_type") == "attachment"]
        ) != len(attachments):
            raise CatabolicError("mux output lost a required font attachment")
        if attachments:
            os.lseek(output_fd, 0, os.SEEK_SET)
            raw = command_output(
                [
                    identity["ffprobe"]["tool"],
                    "-v",
                    "error",
                    "-protocol_whitelist",
                    "file,pipe",
                    "-format_whitelist",
                    "matroska",
                    "-show_streams",
                    "-show_data_hash",
                    "sha256",
                    "-of",
                    "json",
                    f"/dev/fd/{output_fd}",
                ],
                pass_fds=(output_fd,),
                timeout=recipe["timeout"],
                on_poll=poll,
            )
            hashes = [
                s.get("extradata_hash", "").lower().removeprefix("sha256:")
                for s in json.loads(raw)["streams"]
                if s.get("codec_type") == "attachment"
            ]
            if hashes != [a["sha256"] for a in attachments]:
                raise CatabolicError("mux changed required font attachment bytes")
        for source, target in zip(selected, streams, strict=True):
            for name in ("language", "title"):
                if source.get("tags", {}).get(name) not in (
                    None,
                    target.get("tags", {}).get(name),
                ):
                    raise CatabolicError("mux lost selected stream metadata")
            for flag in (
                "default",
                "forced",
                "comment",
                "hearing_impaired",
                "visual_impaired",
            ):
                if source.get("disposition", {}).get(flag) not in (
                    None,
                    target.get("disposition", {}).get(flag),
                ):
                    raise CatabolicError("mux changed selected stream flags")
        verified = []
        for i, (entry, out) in enumerate(
            zip(package["components"], streams, strict=True)
        ):
            source_packets = packet_evidence(
                handles[i],
                entry["occurrence"]["locator"]["index"],
                identity,
                poll,
                recipe["timeout"],
            )
            target_packets = packet_evidence(
                output_fd, out["index"], identity, poll, recipe["timeout"]
            )
            verified.append(
                verify_packets(
                    source_packets,
                    target_packets,
                    entry["compatibility"]["offset_seconds"],
                )
            )
        size = os.fstat(output_fd).st_size
        if not 0 < size < recipe["max_output_bytes"]:
            raise CatabolicError("mux output reached its byte budget or is empty")
        return {
            "operation": "component-mux",
            "output": after,
            "component_signature": package["signature"],
            "components": [
                {
                    "occurrence_id": entry["occurrence"]["occurrence_id"],
                    "output_stream_index": out["index"],
                    "relationship": "mux_copy",
                }
                for entry, out in zip(package["components"], streams, strict=True)
            ],
            "dependencies": attachments,
            "packet_verification": verified,
            "size": size,
        }


def record_lineage(app, artifact, validation, package):
    rows = validation.get("components", [])
    if len(rows) != len(package["components"]):
        raise CatabolicError("component output lineage verification is incomplete")
    for ordinal, row in enumerate(rows):
        app.store.db.execute(
            "INSERT OR IGNORE INTO component_output_lineage VALUES (?,?,?,?,?,?)",
            (
                artifact["id"],
                ordinal,
                row["occurrence_id"],
                row["output_stream_index"],
                row["relationship"],
                package["components"][ordinal]["occurrence"]["component_id"],
            ),
        )


def extraction(
    app, identifier, operation_id, location, *, apply=False, expected_plan=None
):
    from .artifacts import Artifacts

    row = components.get(app.store, app.profile, identifier, require_current=True)
    if not row["technically_verified"] or row["dependencies"]:
        raise CatabolicError(
            "extraction requires verified technical evidence and no unsupported dependencies"
        )
    package = {
        "version": 1,
        "video": {
            "file_id": row["file_id"],
            "revision": row["revision"],
            "occurrence_id": identifier,
        },
        "components": [
            {
                "requirement": "component",
                "occurrence": row,
                "compatibility": {"offset_seconds": 0.0},
            }
        ],
        "dependencies": [],
        "packaging": {
            "action": "extract",
            "ready": False,
            "operation_id": operation_id,
            "exposes_unselected_components": False,
        },
    }
    package["signature"] = hashlib.sha256(encode(package).encode()).hexdigest()
    recipe = Artifacts(app).get_recipe(operation_id)
    index = row["locator"].get("index")
    if recipe["preset"] == "remux-mkv":
        if recipe["definition"].get("stream_indices") != [index]:
            raise CatabolicError(
                "approved remux operation must select exactly this component"
            )
    else:
        from .component_sql import OCCURRENCES_SQL

        siblings = app.store.rows(
            "SELECT locator FROM ("
            + OCCURRENCES_SQL
            + ") WHERE profile=? AND file_id=? AND association_id=? AND revision=? AND kind=? AND current=1",
            (
                app.profile,
                row["file_id"],
                row["association_id"],
                row["revision"],
                row["kind"],
            ),
        )
        indexes = sorted(json.loads(s["locator"]).get("index") for s in siblings)
        ordinal = recipe["definition"].get("stream", 0)
        if ordinal >= len(indexes) or indexes[ordinal] != index:
            raise CatabolicError(
                "approved extraction operation selects a different stream"
            )
    validate(app.store, app.profile, package, recipe)
    if not apply:
        return {"plan_id": package["signature"], "package": package}
    if expected_plan != package["signature"]:
        raise CatabolicError("stale_or_missing_component_extraction_plan")
    return Artifacts(app).enqueue(
        row["file_id"],
        operation_id,
        location,
        row["item_id"],
        _component_package=package,
    )


def render_selected(input_fd, output_fd, recipe, identity, poll, package):
    from . import rendering

    if package["packaging"]["action"] == "mux":
        return render_mux(output_fd, recipe, identity, poll, package)
    row = package["components"][0]["occurrence"]
    before = rendering.probe(input_fd, identity)
    index = row["locator"].get("index")
    if row["locator"]["convention"] != "ffprobe_absolute_stream_index":
        raise CatabolicError("extraction needs an exact stream locator")
    if recipe["preset"] == "remux-mkv":
        if recipe.get("stream_indices") != [index]:
            raise CatabolicError(
                "approved remux operation must select exactly this component"
            )
    else:
        streams = rendering.streams(before, row["kind"])
        selected = recipe.get("stream", 0)
        if selected >= len(streams) or streams[selected]["index"] != index:
            raise CatabolicError(
                "approved extraction operation selects a different stream"
            )
    result = rendering.render(input_fd, output_fd, recipe, identity, poll)
    if len(result["output"]["streams"]) != 1:
        raise CatabolicError("component extraction exposed additional streams")
    if result["output"]["streams"][0].get("codec_type") != row["kind"]:
        raise CatabolicError("component extraction produced the wrong stream kind")
    result["components"] = [
        {
            "occurrence_id": row["occurrence_id"],
            "output_stream_index": result["output"]["streams"][0]["index"],
            "relationship": "extracted_copy"
            if recipe["preset"] == "remux-mkv"
            else "extracted_conversion",
        }
    ]
    result["component_signature"] = package["signature"]
    return result


def resolved_artifact(resolver, primary, package):
    """Reuse only a ready artifact for this exact component/evidence signature."""
    from .artifacts import Artifacts
    from .content_access import authorize, revision_of

    rows = resolver.store.rows(
        "SELECT a.id FROM processing_artifacts a JOIN processing_jobs j ON j.id=a.job_id WHERE a.profile=? AND a.state='ready' AND json_extract(j.options,'$.component_package.signature')=? ORDER BY a.id LIMIT 2",
        (resolver.profile, package["signature"]),
    )
    if not rows:
        return primary
    artifact = Artifacts(resolver.app).get(rows[0]["id"])
    if not Artifacts(resolver.app)._usable(artifact, full=False):
        return primary
    file = resolver.store.rows(
        "SELECT * FROM files WHERE id=?", (artifact["file_id"],)
    )[0]
    rev = revision_of(resolver.store, resolver.profile, file["id"])
    if resolver.access:
        authorize(resolver.access, file["id"], rev)
    package["packaging"] = {
        **package["packaging"],
        "ready": True,
        "artifact_id": artifact["id"],
        "file_id": file["id"],
        "revision": rev,
    }
    primary.update(
        file_id=file["id"],
        path=file["path"],
        location=file["location"],
        revision=rev,
        rendition_id=resolver.store.rows(
            "SELECT id FROM media_outputs WHERE artifact_id=?", (artifact["id"],)
        )[0]["id"],
    )
    primary["checks"].append(occurrence(resolver.store, resolver.profile, file["id"]))
    return primary
