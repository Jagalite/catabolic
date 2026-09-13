# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Verified, explicit, atomic repair of mount numbering; no filesystem writes."""

import hashlib
import json
import os
import time
from contextlib import ExitStack
from pathlib import Path
from uuid import uuid4

from .domain import CatabolicError, source_health
from .filesystem import (
    link_state,
    open_directory,
    owner_state,
    root_handle,
    source_stat,
)
from .reconcile import Reconciler
from .store import encode
from .volume_identity import volume_uuid


def repair(
    app,
    *,
    apply=False,
    adopt_existing=False,
    expected_plan=None,
    trust_sources=(),
    inventory_recovery=False,
):
    if inventory_recovery and (adopt_existing or trust_sources):
        raise CatabolicError("inventory recovery cannot adopt replacement storage")
    app.require_recovered()
    store, profile = app.store, app.profile
    if store.rows(
        "SELECT 1 FROM consumer_deliveries WHERE profile=? AND lease_until>?",
        (profile, time.time()),
    ):
        raise CatabolicError(
            "consumer delivery is in flight; retry after its lease ends"
        )
    if store.rows(
        "SELECT 1 FROM owned_hardlinks WHERE profile=? UNION ALL SELECT 1 FROM retained_hardlinks WHERE profile=? LIMIT 1",
        (profile, profile),
    ):
        raise CatabolicError(
            "remount repair of hardlink ownership is not yet supported"
        )
    bindings = store.rows(
        "SELECT * FROM bindings WHERE profile=? ORDER BY kind,owner", (profile,)
    )
    if not bindings:
        raise CatabolicError("no bindings to repair")
    trusted = set(trust_sources)
    sources = {r["owner"] for r in bindings if r["kind"] == "source"}
    if trusted - sources:
        raise CatabolicError("unknown source in --trust-source")
    if any(
        store.rows(
            "SELECT 1 FROM generated_locations WHERE profile=? AND location=?",
            (profile, source),
        )
        for source in trusted
    ):
        raise CatabolicError(
            "generated artifact locations cannot be adopted with --trust-source"
        )
    rows, candidates, handles, observations = [], {}, {}, {}
    pending_sources, output_links, selections = [], {}, []
    with ExitStack() as stack:
        for old in bindings:
            key = old["kind"], old["owner"]
            fd = open_directory(old["root"])
            stack.callback(os.close, fd)
            st = os.fstat(fd)
            stable = volume_uuid(fd)
            trust = old["kind"] == "source" and old["owner"] in trusted
            if not stable and not trust:
                raise CatabolicError("stable volume UUID unavailable; repair refused")
            if st.st_ino != old["inode"] and not trust:
                raise CatabolicError(
                    "root inode changed; remount repair cannot adopt a replacement directory"
                )
            known = store.rows(
                "SELECT volume_uuid FROM binding_volumes WHERE profile=? AND kind=? AND owner=?",
                (profile, *key),
            )
            if known and known[0]["volume_uuid"] != stable and not trust:
                raise CatabolicError(
                    "volume UUID changed; remount repair cannot adopt a replacement volume"
                )
            if not known and not adopt_existing and not trust:
                raise CatabolicError(
                    "legacy binding has no volume UUID; preview with --adopt-existing"
                )
            if old["kind"] == "output":
                if app.link_mode(old["owner"]) != "symlink":
                    raise CatabolicError(
                        "remount repair currently supports symlink outputs only"
                    )
                if owner_state(fd, Reconciler(app).owner(old["owner"])) != "owned":
                    raise CatabolicError("output ownership is missing")
            candidate = {**old, "device": st.st_dev, "inode": st.st_ino}
            rows.append(
                {
                    "before": old,
                    "after": candidate,
                    "volume_uuid": stable,
                    "previous_volume_uuid": known[0]["volume_uuid"] if known else None,
                    "adopted": not bool(known),
                    "trusted_replacement": trust,
                }
            )
            candidates[key], handles[key] = candidate, fd
        count = 0
        for row in rows:
            old = row["before"]
            if old["kind"] != "output":
                continue
            catalog = old["owner"]
            owned = {
                r["path"]: r["target"]
                for r in store.rows(
                    "SELECT path,target FROM owned_links WHERE profile=? AND catalog=?",
                    (profile, catalog),
                )
            }
            mappings = store.rows(
                """SELECT m.path,m.file_id,f.location,f.path AS source_path,o.*
                FROM mappings m JOIN files f ON f.id=m.file_id
                LEFT JOIN observations o ON o.file_id=f.id AND o.profile=?
                WHERE m.catalog=? AND m.active=1 ORDER BY m.path""",
                (profile, catalog),
            )
            if set(owned) != {m["path"] for m in mappings}:
                raise CatabolicError(
                    "owned links differ from selected mappings; repair refused"
                )
            for mapping in mappings:
                selections.append(
                    {
                        "catalog": catalog,
                        **{
                            key: mapping[key]
                            for key in ("path", "file_id", "location", "source_path")
                        },
                    }
                )
                key = "source", mapping["location"]
                if key not in candidates:
                    raise CatabolicError("published source is unbound")
                expected = os.path.relpath(
                    Path(candidates[key]["root"]) / mapping["source_path"],
                    (Path(old["root"]) / mapping["path"]).parent,
                )
                if owned[mapping["path"]] != expected or link_state(
                    handles[("output", catalog)], mapping["path"]
                ) != ("link", expected):
                    raise CatabolicError(
                        "published symlink or target changed; repair refused"
                    )
                output_links.setdefault(catalog, {})[mapping["path"]] = expected
                count += 1
                try:
                    current = source_stat(handles[key], mapping["source_path"])
                    trust = mapping["location"] in trusted
                    if source_health(current.st_size, mapping["source_path"]):
                        raise CatabolicError(
                            "published source is unhealthy; repair refused"
                        )
                    if mapping["status"] is None:
                        raise CatabolicError(
                            "published source has no observation; scan first"
                        )
                    if not trust and (
                        mapping["status"] != "present"
                        or (
                            current.st_size,
                            current.st_mtime_ns,
                            current.st_ino,
                            current.st_dev,
                        )
                        != (
                            mapping["size"],
                            mapping["mtime_ns"],
                            mapping["inode"],
                            candidates[key]["device"],
                        )
                    ):
                        raise CatabolicError(
                            "published source metadata changed; repair refused"
                        )
                except (OSError, CatabolicError) as exc:
                    if not inventory_recovery:
                        raise
                    pending_sources.append(
                        {
                            "file_id": mapping["file_id"],
                            "source": mapping["location"],
                            "path": mapping["source_path"],
                            "reason": str(exc),
                        }
                    )
                    continue
                observations[mapping["file_id"]] = {
                    "device": current.st_dev,
                    "inode": current.st_ino,
                    "size": current.st_size,
                    "mtime_ns": current.st_mtime_ns,
                    "old_device": mapping["device"],
                    "trusted_replacement": trust,
                }
        plan = {
            "bindings": rows,
            "verified_links": count,
            "observations": observations,
            "inventory_recovery": inventory_recovery,
            "pending_sources": pending_sources,
        }
        # Automatic repair authorizes storage renumbering, not a frozen file
        # inventory. The apply pass independently classifies current file facts.
        # Keep the strict explicit-remount contract over the full evidence plan.
        preconditions = (
            {
                "bindings": rows,
                "output_links": output_links,
                "selections": selections,
                "inventory_recovery": True,
            }
            if inventory_recovery
            else plan
        )
        digest = hashlib.sha256(encode(preconditions).encode()).hexdigest()
        result = {
            "plan_id": digest,
            "bindings": rows,
            "verified_links": count,
            "verified_sources": len(observations),
            "sources_needing_observation": pending_sources,
            "inventory_recovery": inventory_recovery,
            "applied": False,
            "complete": True,
        }
        if not apply:
            return result
        if expected_plan != digest:
            raise CatabolicError(
                "repair preview changed; preview again and supply --expected-plan"
            )
        with store.transaction() as db:
            db.execute("INSERT INTO remount_guard VALUES (?)", (profile,))
            for row in rows:
                old, new = row["before"], row["after"]
                fd = open_directory(old["root"])
                try:
                    if (os.fstat(fd).st_dev, os.fstat(fd).st_ino, volume_uuid(fd)) != (
                        new["device"],
                        new["inode"],
                        row["volume_uuid"],
                    ):
                        raise CatabolicError("root changed during repair")
                finally:
                    os.close(fd)
                db.execute(
                    "UPDATE bindings SET device=?,inode=? WHERE profile=? AND kind=? AND owner=?",
                    (new["device"], new["inode"], profile, old["kind"], old["owner"]),
                )
                db.execute(
                    "DELETE FROM binding_volumes WHERE profile=? AND kind=? AND owner=?",
                    (profile, old["kind"], old["owner"]),
                )
                if row["volume_uuid"]:
                    db.execute(
                        "INSERT INTO binding_volumes VALUES (?,?,?,?)",
                        (profile, old["kind"], old["owner"], row["volume_uuid"]),
                    )
                if old["kind"] == "output" and old != new:
                    for consumer in store.rows(
                        "SELECT id,local_binding FROM consumer_bindings WHERE profile=? AND catalog=?",
                        (profile, old["owner"]),
                    ):
                        if json.loads(consumer["local_binding"]) != old:
                            raise CatabolicError(
                                "consumer pinned to different output; repair refused"
                            )
                    db.execute(
                        "UPDATE consumer_bindings SET local_binding=?,revision=revision+1,indexing=NULL WHERE profile=? AND catalog=?",
                        (encode(new), profile, old["owner"]),
                    )
                    publications = store.rows(
                        "SELECT local_binding FROM publication_generations WHERE profile=? AND catalog=?",
                        (profile, old["owner"]),
                    )
                    if (
                        publications
                        and json.loads(publications[0]["local_binding"]) != old
                    ):
                        raise CatabolicError(
                            "publication pinned to different output; repair refused"
                        )
                    db.execute(
                        "UPDATE publication_generations SET local_binding=? WHERE profile=? AND catalog=?",
                        (encode(new), profile, old["owner"]),
                    )
            for file_id, observation in observations.items():
                if not observation["trusted_replacement"]:
                    db.execute(
                        "UPDATE observations SET device=? WHERE profile=? AND file_id=?",
                        (observation["device"], profile, file_id),
                    )
            db.execute("DELETE FROM remount_guard WHERE profile=?", (profile,))
            # Replacement media is a new observed version: normal publication triggers apply.
            for file_id, observation in observations.items():
                if observation["trusted_replacement"]:
                    db.execute(
                        "UPDATE observations SET device=?,inode=?,size=?,mtime_ns=?,status='present' WHERE profile=? AND file_id=?",
                        (
                            observation["device"],
                            observation["inode"],
                            observation["size"],
                            observation["mtime_ns"],
                            profile,
                            file_id,
                        ),
                    )
            # Catalog definitions are global, but output bindings and this repair
            # are profile-local. Verify exactly the outputs pinned in the plan.
            reports = []
            for row in rows:
                if row["before"]["kind"] == "output":
                    reports.extend(
                        Reconciler(app).verify(row["before"]["owner"])["catalogs"]
                    )
            verification = {
                "healthy": all(report["healthy"] for report in reports),
                "catalogs": reports,
            }
            if inventory_recovery:
                # File health belongs to observation and synchronization. Root
                # identity and output ownership remain mandatory at commit.
                for row in rows:
                    bound = app.binding(row["before"]["kind"], row["before"]["owner"])
                    with root_handle(bound) as fd:
                        current = os.fstat(fd)
                        expected = row["after"]
                        if (current.st_dev, current.st_ino, volume_uuid(fd)) != (
                            expected["device"],
                            expected["inode"],
                            row["volume_uuid"],
                        ):
                            raise CatabolicError("root changed during repair")
                        if row["before"]["kind"] == "output":
                            catalog = row["before"]["owner"]
                            if (
                                owner_state(fd, Reconciler(app).owner(catalog))
                                != "owned"
                            ):
                                raise CatabolicError(
                                    "output ownership changed during repair"
                                )
                            for path, target in output_links.get(catalog, {}).items():
                                if link_state(fd, path) != ("link", target):
                                    raise CatabolicError(
                                        "published symlink changed during repair"
                                    )
            if not verification["healthy"] and not inventory_recovery:
                reasons = [
                    {"catalog": report["catalog"], **issue}
                    for report in reports
                    for issue in report["issues"]
                ]
                raise CatabolicError(
                    "final publication verification failed; repair rolled back: "
                    + encode(reasons[:10])
                )
            db.execute("DELETE FROM remount_guard WHERE profile=?", (profile,))
            db.execute(
                "INSERT INTO remount_repairs(id,profile,plan) VALUES (?,?,?)",
                (str(uuid4()), profile, encode(plan)),
            )
        return {**result, "applied": True, "verification": verification}


def recover_device_numbers(app, *, required_bindings=None):
    """Reverify mount renumbering through the normal audited repair service."""
    changed = False
    for binding in app.store.rows(
        "SELECT * FROM bindings WHERE profile=? ORDER BY kind,owner", (app.profile,)
    ):
        if (
            required_bindings is not None
            and (binding["kind"], binding["owner"]) not in required_bindings
        ):
            continue
        if binding["kind"] == "source":
            from .source_trust import effective

            if effective(app, binding["owner"])["settings"]["device"] == "skip":
                continue
        fd = open_directory(binding["root"])
        try:
            if os.fstat(fd).st_dev != binding["device"]:
                changed = True
        finally:
            os.close(fd)
    if not changed:
        return None
    # No legacy adoption or trust override: both passes require stable identity,
    # ownership, and the same exact repair plan. Changed files await observation.
    preview = repair(app, inventory_recovery=True)
    result = repair(
        app, apply=True, expected_plan=preview["plan_id"], inventory_recovery=True
    )
    return {**result, "automatic": True}
