# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Selection-based folder, immutable export, and CLI-import mapping adapters.

The owners of layouts, links, manifest serialization, and source staging remain
responsible for their mutations. This layer adds reviewed plans and durable
cross-operation outcomes without pretending every destination supports edits.
"""

import copy
import json
import os
import shutil
import stat
from collections import defaultdict
from pathlib import Path
from uuid import uuid4

from .app import Application
from .destination_mappings import digest, fail
from .exports import build_export, publish_bundle
from .filesystem import open_directory, root_handle
from .importers import import_catalog, import_source
from .layouts import Layouts
from .manifest import Manifest
from .network_adapters import request, token
from .projections import Projections
from .reconcile import Reconciler
from .saved_queries import Queries
from .source_access import validated_source
from .store import Store, encode
from .targets import definitions

IMPORTS = {"calibre", "calibre-web", "immich"}
EXPORTS = {"nfo", "opds", "xspf"}
TERMINAL = {"verified", "submitted", "not_applied"}
MAX_PLAN = 4 * 1024 * 1024


class Rollback(Exception):
    pass


def capabilities():
    result = {}
    for adapter in definitions():
        result[adapter] = {
            "folder": {
                "definition_version": 2,
                "query": "selection",
                "fields": "catalog metadata and preset naming rules",
                "ownership": "managed links only",
                "verification": "filesystem",
                "recovery": "reconciler journal",
                "source_mutation": False,
            }
        }
    for adapter in IMPORTS:
        result[adapter] = {
            "import": {
                "definition_version": 2,
                "query": "selection",
                "fields": ["title", "author"] if adapter != "immich" else [],
                "ownership": "submitted source revisions",
                "verification": "CLI completion only",
                "recovery": "inspect destination and attest outcome",
                "copies_media": True,
                "metadata_updates": False,
                "source_mutation": False,
            }
        }
    for adapter in EXPORTS:
        result[adapter] = {
            "export": {
                "definition_version": 2,
                "query": "selection",
                "fields": "existing export format schema",
                "ownership": "new immutable bundle",
                "verification": "exact files and links",
                "recovery": "resume missing files in the recorded bundle; no overwrite or deletion",
                "source_mutation": False,
            }
        }
    return result


def validate(definition):
    if (
        not isinstance(definition, dict)
        or set(definition) != {"version", "id", "query", "destination"}
        or type(definition["version"]) is not int
        or definition["version"] != 2
    ):
        fail("invalid_publication_definition")
    from .consumer_adapters import text

    text(definition["id"], 255)
    text(definition["query"], 255)
    dest = definition["destination"]
    if not isinstance(dest, dict) or not {"adapter", "operation", "catalog"} <= set(
        dest
    ):
        fail("invalid_publication_destination")
    for k in ("adapter", "operation", "catalog"):
        text(dest[k], 255)
    op, adapter = dest["operation"], dest["adapter"]
    if op not in capabilities().get(adapter, {}):
        fail("unsupported_destination_operation")
    extras = {
        "folder": {"allow_empty"},
        "export": {"path", "base_url"},
        "import": {"path"},
    }[op]
    if set(dest) - {"adapter", "operation", "catalog"} - extras:
        fail("unsupported_destination_options")
    if op != "folder":
        text(dest.get("path"))
    if "allow_empty" in dest and type(dest["allow_empty"]) is not bool:
        fail("invalid_allow_empty")
    return dest


def history(store, profile):
    return [
        dict(id=r["id"], **json.loads(r["subject"]))
        for r in store.rows(
            "SELECT id,subject FROM consumer_events WHERE profile=? AND event='mapping_publish' ORDER BY rowid",
            (profile,),
        )
        if json.loads(r["subject"]).get("version") == 2
    ]


def record(store, profile, identifier, payload, *, create=False):
    from .destination_mappings import write_event

    write_event(store, profile, identifier, payload, create=create)


def pending(store, profile, scope):
    events = [e for e in history(store, profile) if e["scope"] == scope]
    unresolved = [e for e in events if e["state"] not in TERMINAL]
    if unresolved:
        fail("unresolved_write_recover_" + unresolved[0]["id"])
    return events


def root_identity(path):
    fd = open_directory(path)
    try:
        s = os.fstat(fd)
        return {"device": s.st_dev, "inode": s.st_ino}
    finally:
        os.close(fd)


def document_sources(app, document):
    snapshots = {}
    for file in document["content"]["files"]:
        snapshot = import_source(app, file)
        with validated_source(snapshot) as fd:
            snapshot["ctime_ns"] = os.fstat(fd).st_ctime_ns
        snapshots[file["id"]] = snapshot
    return snapshots


def source_check(snapshots):
    for snapshot in snapshots.values():
        with validated_source(snapshot):
            pass


def folder_state(app, catalog):
    return {
        "projection": Projections(app).get(catalog),
        "mappings": app.store.rows(
            "SELECT * FROM mappings WHERE catalog=? ORDER BY id", (catalog,)
        ),
        "output": dict(app.binding("output", catalog)),
    }


def stage_folder(app, definition):
    dest = definition["destination"]
    catalog = dest["catalog"]
    # Reuse the established owner; never silently take over an existing projection.
    layout_name = "mapping-" + digest([definition["id"], dest["adapter"], catalog])[:24]
    prior = Projections(app).get(catalog)
    if prior["layout"] not in (None, layout_name):
        fail("folder_owned_by_another_projection")
    from .fallback_projection import binding

    if binding(app.store, app.profile, catalog):
        fail("folder_has_fallback_owner")
    if app.link_mode(catalog) != "symlink":
        fail("folder_mapping_requires_symlink_catalog")
    Layouts(app).put(layout_name, definitions()[dest["adapter"]])
    Projections(app).put(catalog, definition["query"], layout_name)
    layout = Layouts(app).run(
        layout_name,
        catalog,
        apply=True,
        allow_empty=dest.get("allow_empty", False),
        limit=1000,
    )
    if layout["details_truncated"]:
        fail("folder_plan_exceeds_limit")
    reconciliation = Reconciler(app, notify_consumers=False).preview(catalog)
    if len(reconciliation["actions"]) > 1000:
        fail("folder_plan_exceeds_limit")
    blocked = any(a["kind"] == "blocked_source" for a in reconciliation["actions"])
    safe = layout["safe"] and reconciliation["safe"] and not blocked
    return {
        "layout": layout,
        "reconciliation": reconciliation,
        "safe": safe,
        "state": folder_state(app, catalog),
    }


def group_document(document, associations):
    from .interchange.validation import validate_document

    subset = copy.deepcopy(document)
    content = subset["content"]
    file_ids = {a["file_id"] for a in associations}
    association_ids = {a["id"] for a in associations}
    content["associations"] = associations
    content["files"] = [f for f in content["files"] if f["id"] in file_ids]
    content["entries"] = [
        e for e in content["entries"] if set(e["association_ids"]) & association_ids
    ]
    for entry in content["entries"]:
        entry["association_ids"] = [
            a for a in entry["association_ids"] if a in association_ids
        ]
    paths = {e["path"] for e in content["entries"]}
    for key in ("recorded_links", "hardlinks", "retained_hardlinks"):
        content[key] = [e for e in content[key] if e["path"] in paths]
    content["taggings"] = [
        t
        for t in content["taggings"]
        if t["subject_type"] != "file" or t["subject_id"] in file_ids
    ]
    content["counts"] = {key: len(content[key]) for key in content["counts"]}
    subset["content_sha256"] = digest(content)
    validate_document(subset)
    return subset


def import_identity(destination):
    return root_identity(destination) if os.path.lexists(destination) else None


def immich_identity(destination):
    """Resolve the credential owner without persisting credentials or user details."""
    base = destination.rstrip("/")
    if not base.endswith("/api"):
        base += "/api"
    try:
        user = json.loads(
            request(
                base + "/users/me",
                headers={
                    "x-api-key": token("IMMICH_API_KEY"),
                    "Accept": "application/json",
                },
                structured_errors=True,
            )
        )
    except (ValueError, TypeError):
        fail("invalid_import_account_identity")
    identifier = user.get("id") if isinstance(user, dict) else None
    if not isinstance(identifier, str) or not identifier or len(identifier) > 128:
        fail("invalid_import_account_identity")
    return {"user_id": identifier}


def import_signature(sources, items, associations, adapter):
    # Only transmitted metadata and actual file revisions determine deduplication.
    # A rescan or local workflow annotation is not a new remote import.
    files = [
        {
            k: source[k]
            for k in (
                "id",
                "location",
                "path",
                "root",
                "device",
                "inode",
                "size",
                "mtime_ns",
                "ctime_ns",
            )
        }
        for _, source in sorted(sources.items())
    ]
    metadata = [
        {
            "id": item["id"],
            "kind": item["kind"],
            **(
                {
                    "title": item["metadata"].get("title") or item["id"],
                    "author": item["metadata"].get("author"),
                }
                if adapter != "immich"
                else {}
            ),
        }
        for item in items
    ]
    links = [
        {k: a[k] for k in ("item_id", "file_id", "role", "part")} for a in associations
    ]
    return digest([files, metadata, links])


def prepare(app, definition):
    dest = validate(definition)
    query = Queries(app.store, app.profile).get(definition["query"])
    if query["definition"]["mode"] != "selection":
        fail("publication_requires_selection_query")
    op = dest["operation"]
    plan = {
        "definition": definition,
        "database_id": app.store.database_id,
        "profile": app.profile,
        "query": query,
    }
    if op == "folder":
        app.require_recovered()
        bound = app.binding("output", dest["catalog"])
        with root_handle(bound):
            pass
        scope = ["folder", dest["catalog"]]
        pending(app.store, app.profile, scope)
        # Preview stages only SQLite state and rolls all of it back.
        try:
            with app.store.transaction():
                staged = stage_folder(app, definition)
                raise Rollback()
        except Rollback:
            pass
        plan.update(
            scope=scope,
            data=staged,
            safe=staged["safe"],
            removals=staged["reconciliation"]["removal_budget"]["removals"],
        )
    else:
        with app.store.transaction():
            document = Manifest(app).build(
                dest["catalog"],
                selection={"query_id": definition["query"], "profile": app.profile},
            )
        if len(encode(document).encode()) > MAX_PLAN:
            fail("publication_plan_exceeds_limit")
        snapshots = document_sources(app, document)
        plan.update(document=document, sources=snapshots, safe=True, removals=0)
        if op == "export":
            output = Path(dest["path"]).absolute()
            app._validate_binding_path("output", "export", output)
            parent = root_identity(output.parent)
            scope = ["export", str(output)]
            past = pending(app.store, app.profile, scope)
            export = build_export(document, dest["adapter"], dest.get("base_url"))
            # The low-level publisher's complete preflight must also run in preview.
            from .exports import validate_bundle

            validate_bundle(app, document, export, str(output))
            spec = {
                "document": document,
                "export": export,
                "sources": snapshots,
                "output": str(output),
                "parent": parent,
            }
            signature = digest({**spec, "document": stable_document(document)})
            previous = [e for e in past if e["state"] == "verified"]
            if previous:
                event = previous[-1]
                if (
                    event["signature"] != signature
                    or event["owner"] != definition["id"]
                ):
                    fail("immutable_export_choose_new_destination")
                if not verify_bundle(event):
                    fail("owned_export_changed")
            elif os.path.lexists(output):
                fail("export_destination_exists")
            plan.update(
                scope=scope, data=spec, signature=signature, unchanged=bool(previous)
            )
        else:
            preview = import_catalog(
                app, document, dest["adapter"], dest["path"], limit=1000
            )
            destination = preview["destination"]
            parent = (
                None
                if dest["adapter"] == "immich"
                else root_identity(Path(destination).parent)
            )
            scope = [
                "import",
                "calibre" if dest["adapter"] == "calibre-web" else dest["adapter"],
                destination,
            ]
            if dest["adapter"] == "immich":
                # Old URL-only records cannot safely be attributed to this user.
                legacy = pending(app.store, app.profile, scope)
                if any(e["state"] == "submitted" for e in legacy):
                    fail("legacy_import_account_identity_unknown")
                destination_identity = immich_identity(destination)
                scope.append(destination_identity["user_id"])
            else:
                destination_identity = import_identity(destination)
            past = pending(app.store, app.profile, scope)
            submitted = [e for e in past if e["state"] == "submitted"]
            if (
                parent is not None
                and submitted
                and submitted[-1].get("destination_identity") != destination_identity
            ):
                fail("import_destination_identity_changed")
            groups = defaultdict(list)
            items = {i["id"]: i for i in document["content"]["items"]}
            supported = (
                {"photo", "video"}
                if dest["adapter"] == "immich"
                else {"book", "book_edition"}
            )
            for a in document["content"]["associations"]:
                if (
                    a["role"] != "primary"
                    or items[a["item_id"]]["kind"] not in supported
                ):
                    fail("unsupported_import_selection")
                groups[
                    a["file_id"] if dest["adapter"] == "immich" else a["item_id"]
                ].append(a)
            planned = []
            planned_bytes = 0
            for key, associations in sorted(groups.items()):
                file_ids = {a["file_id"] for a in associations}
                item_ids = {a["item_id"] for a in associations}
                subset = group_document(document, associations)
                group_sources = {k: v for k, v in snapshots.items() if k in file_ids}
                signature = import_signature(
                    group_sources,
                    [i for i in subset["content"]["items"] if i["id"] in item_ids],
                    associations,
                    dest["adapter"],
                )
                previous = [
                    e for e in past if e["key"] == key and e["state"] == "submitted"
                ]
                if previous and previous[-1]["signature"] != signature:
                    fail("import_revision_changed_updates_unsupported")
                planned.append(
                    {
                        "key": key,
                        "document": subset,
                        "sources": group_sources,
                        "signature": signature,
                        "unchanged": bool(previous),
                    }
                )
                planned_bytes += len(encode(planned[-1]).encode())
                if planned_bytes > MAX_PLAN:
                    fail("publication_plan_exceeds_limit")
            plan.update(
                executable=shutil.which(preview["executable"]),
                destination_identity=destination_identity,
                scope=scope,
                destination=destination,
                parent=parent,
                data=planned,
                actions=preview["actions"],
            )
    if len(encode(plan).encode()) > MAX_PLAN:
        fail("publication_plan_exceeds_limit")
    return plan


def stable_document(document):
    return {k: v for k, v in document.items() if k != "generated_at"}


def plan_digest(plan):
    value = copy.deepcopy(plan)
    if "document" in value:
        value["document"] = stable_document(value["document"])
    data = value.get("data")
    if isinstance(data, dict) and "document" in data:
        data["document"] = stable_document(data["document"])
    elif isinstance(data, list):
        for group in data:
            group["document"] = stable_document(group["document"])
    return digest(value)


def bundle_entries(data):
    output = Path(data["output"])
    expected = {
        e["path"]: (
            "link",
            os.path.relpath(
                str(
                    Path(data["sources"][e["file_id"]]["root"])
                    / data["sources"][e["file_id"]]["path"]
                ),
                output / Path(e["path"]).parent,
            ),
        )
        for e in data["document"]["content"]["entries"]
    }
    for artifact in data["export"]["files"]:
        expected[artifact["path"]] = ("file", artifact["content"])
    expected["catalog-manifest.json"] = (
        "file",
        json.dumps(
            data["document"], ensure_ascii=False, allow_nan=False, sort_keys=True
        )
        + "\n",
    )
    return expected


def verify_bundle(event, *, allow_missing=False):
    """Read-only comparison; never adopt or repair an unexpected path."""
    data = event["data"]
    output = Path(data["output"])
    try:
        if (
            not event.get("output_identity")
            or root_identity(output) != event["output_identity"]
        ):
            return False
        if root_identity(output.parent) != data["parent"]:
            return False
        expected = bundle_entries(data)
        seen = set()
        parents = {
            str(p) for key in expected for p in Path(key).parents if str(p) != "."
        }
        for parent, directories, files in os.walk(output, followlinks=False):
            for name in directories[:]:
                p = Path(parent) / name
                if p.is_symlink():
                    files.append(name)
                    directories.remove(name)
                elif str(p.relative_to(output)) not in parents:
                    return False
            for name in files:
                p = Path(parent) / name
                relative = str(p.relative_to(output))
                if relative not in expected:
                    return False
                kind, value = expected[relative]
                s = p.lstat()
                if kind == "link":
                    if not stat.S_ISLNK(s.st_mode) or os.readlink(p) != value:
                        return False
                else:
                    fd = os.open(p, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
                    try:
                        if not stat.S_ISREG(os.fstat(fd).st_mode) or os.fstat(
                            fd
                        ).st_size != len(value.encode()):
                            return False
                        with os.fdopen(fd, "rb", closefd=False) as stream:
                            if stream.read(len(value.encode()) + 1) != value.encode():
                                return False
                    finally:
                        os.close(fd)
                seen.add(relative)
        return (
            seen <= set(expected) if allow_missing else seen == set(expected)
        ) and root_identity(output) == event["output_identity"]
    except OSError:
        return False


def resume_bundle(event):
    from .filesystem import parent_handle

    if not verify_bundle(event, allow_missing=True):
        fail("partial_export_changed")
    data = event["data"]
    source_check(data["sources"])
    root = open_directory(data["output"])
    try:
        st = os.fstat(root)
        if {"device": st.st_dev, "inode": st.st_ino} != event["output_identity"]:
            fail("export_identity_changed")
        for relative, (kind, value) in bundle_entries(data).items():
            with parent_handle(root, relative, create=True) as (parent, leaf):
                try:
                    os.stat(leaf, dir_fd=parent, follow_symlinks=False)
                except FileNotFoundError:
                    if kind == "link":
                        os.symlink(value, leaf, dir_fd=parent)
                    else:
                        fd = os.open(
                            leaf,
                            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                            0o644,
                            dir_fd=parent,
                        )
                        with os.fdopen(fd, "wb") as stream:
                            stream.write(value.encode())
                            stream.flush()
                            os.fsync(stream.fileno())
                    os.fsync(parent)
        os.fsync(root)
    finally:
        os.close(root)
    source_check(data["sources"])


def event_base(definition, plan, key):
    return {
        "version": 2,
        "owner": definition["id"],
        "key": key,
        "definition": definition,
        "scope": plan["scope"],
        "state": "started",
        "plan_id": plan_digest(plan),
        "database_id": plan["database_id"],
    }


def run(
    database, profile, definition, *, apply=False, expected_plan=None, max_removals=0
):
    if apply and not expected_plan:
        fail("apply_requires_expected_plan")
    if type(max_removals) is not int or max_removals < 0:
        fail("invalid_removal_budget")
    with Store(database, writable=True) as store:
        app = Application(store, profile)
        plan = prepare(app, definition)
        result = {
            "plan_id": plan_digest(plan),
            "mapping": definition["id"],
            "operation": definition["destination"]["operation"],
            "complete": plan["safe"],
            "safe": plan["safe"],
            "applied": False,
            "removals": plan["removals"],
            "plan": plan,
            "results": [],
        }
        if not apply:
            return result
        if expected_plan != result["plan_id"]:
            fail("stale_plan_preview_again")
        if not plan["safe"]:
            return result
        if plan["removals"] > max_removals:
            fail("removal_budget_exceeded")
        dest = definition["destination"]
        op = dest["operation"]
        if op == "folder":
            identifier = str(uuid4())
            with store.transaction():
                staged = stage_folder(app, definition)
                if staged != plan["data"]:
                    fail("folder_changed_preview_again")
                event = event_base(definition, plan, dest["catalog"])
                event["data"] = staged
                record(store, profile, identifier, event, create=True)
            try:
                execution = Reconciler(app).apply(
                    dest["catalog"], max_removals=max_removals
                )
                verified = Reconciler(app).verify(dest["catalog"])
                if not execution["healthy"] or not verified["healthy"]:
                    fail("folder_verification_failed")
                event["state"] = "verified"
                record(store, profile, identifier, event)
                result.update(applied=True, execution=execution, verification=verified)
            except Exception:
                event["state"] = "uncertain"
                record(store, profile, identifier, event)
                result.update(
                    complete=False,
                    event_id=identifier,
                    state="uncertain",
                    safe_to_retry=False,
                )
            result["results"].append({"event_id": identifier, "state": event["state"]})
        elif op == "export":
            if plan["unchanged"]:
                result.update(applied=True, unchanged=True)
                return result
            source_check(plan["sources"])
            if (
                root_identity(Path(plan["data"]["output"]).parent)
                != plan["data"]["parent"]
            ):
                fail("destination_parent_changed")
            identifier = str(uuid4())
            event = {
                **event_base(definition, plan, dest["catalog"]),
                "data": plan["data"],
                "signature": plan["signature"],
            }
            record(store, profile, identifier, event, create=True)

            def created(identity):
                event["output_identity"] = identity
                record(store, profile, identifier, event)

            try:
                publish_bundle(
                    app,
                    plan["document"],
                    plan["data"]["export"],
                    plan["data"]["output"],
                    on_created=created,
                )
                source_check(plan["sources"])
                if not verify_bundle(event):
                    fail("export_verification_failed")
                event["state"] = "verified"
                result["applied"] = True
            except Exception:
                event["state"] = "uncertain"
                result.update(
                    complete=False,
                    event_id=identifier,
                    state="uncertain",
                    safe_to_retry=False,
                )
            record(store, profile, identifier, event)
            result["results"].append({"event_id": identifier, "state": event["state"]})
        else:
            if any(not group["unchanged"] for group in plan["data"]):
                if not plan["executable"]:
                    fail("import_cli_missing")
                if dest["adapter"] == "immich" and not os.environ.get("IMMICH_API_KEY"):
                    fail("import_credentials_missing")
            destination_identity = plan["destination_identity"]
            for group in plan["data"]:
                if group["unchanged"]:
                    result["results"].append(
                        {"key": group["key"], "state": "submitted", "unchanged": True}
                    )
                    continue
                source_check(group["sources"])
                if (
                    dest["adapter"] == "immich"
                    and immich_identity(plan["destination"]) != destination_identity
                ):
                    fail("import_destination_identity_changed")
                if (
                    plan["parent"] is not None
                    and root_identity(Path(plan["destination"]).parent)
                    != plan["parent"]
                ):
                    fail("destination_parent_changed")
                if (
                    plan["parent"] is not None
                    and import_identity(plan["destination"]) != destination_identity
                ):
                    fail("import_destination_identity_changed")
                identifier = str(uuid4())
                event = {
                    **event_base(definition, plan, group["key"]),
                    "destination_identity": destination_identity,
                    "signature": group["signature"],
                    "data": group,
                    "destination": plan["destination"],
                }
                record(store, profile, identifier, event, create=True)
                try:
                    execution = import_catalog(
                        app,
                        group["document"],
                        dest["adapter"],
                        plan["destination"],
                        apply=True,
                        limit=1000,
                    )
                    if plan["parent"] is not None:
                        observed = import_identity(plan["destination"])
                        if (
                            destination_identity is not None
                            and observed != destination_identity
                        ):
                            fail("import_destination_identity_changed")
                        destination_identity = observed
                        event["destination_identity"] = observed
                    event["execution"] = execution
                    event["state"] = (
                        "submitted"
                        if execution["complete"]
                        else "not_applied"
                        if execution.get("safe_to_retry")
                        else "uncertain"
                    )
                    event["verification"] = (
                        "CLI completion only; remote metadata not verified"
                    )
                except Exception:
                    event["state"] = "uncertain"
                record(store, profile, identifier, event)
                result["results"].append(
                    {
                        "event_id": identifier,
                        "key": group["key"],
                        "state": event["state"],
                    }
                )
                if event["state"] != "submitted":
                    result.update(
                        complete=False,
                        event_id=identifier,
                        safe_to_retry=event["state"] == "not_applied",
                    )
                    return result
            result.update(
                applied=True,
                verification="CLI completion only; remote metadata not verified",
            )
        return result


def recover(
    database, profile, identifier, *, apply=False, resolution=None, expected_plan=None
):
    with Store(database, writable=True) as store:
        found = [e for e in history(store, profile) if e["id"] == identifier]
        if len(found) != 1:
            fail("unknown_event")
        event = found[0]
        plan_id = digest(event)
        if event["database_id"] != store.database_id:
            fail("event_database_changed")
        if event["state"] in TERMINAL:
            return {"event_id": identifier, "state": event["state"], "complete": True}
        op = event["definition"]["destination"]["operation"]
        state = "uncertain"
        destination_identity = event.get("destination_identity")
        if op == "import":
            if event["definition"]["destination"]["adapter"] == "immich":
                if destination_identity is None:
                    fail("legacy_import_account_identity_unknown")
                current = immich_identity(event["destination"])
            else:
                current = import_identity(event["destination"])
            if destination_identity is not None and current != destination_identity:
                fail("import_destination_identity_changed")
            destination_identity = current
            plan_id = digest([event, destination_identity])
        if resolution is not None and (
            op != "import" or resolution not in ("submitted", "not_applied")
        ):
            fail("unsupported_recovery_resolution")
        if op == "export":
            if (
                apply
                and not verify_bundle(event)
                and verify_bundle(event, allow_missing=True)
            ):
                resume_bundle(event)
            if verify_bundle(event):
                source_check(event["data"]["sources"])
                state = "verified"
            elif (
                not event.get("output_identity")
                and not os.path.lexists(event["data"]["output"])
                and root_identity(Path(event["data"]["output"]).parent)
                == event["data"]["parent"]
            ):
                state = "not_applied"
        elif op == "folder":
            app = Application(store, profile)
            catalog = event["definition"]["destination"]["catalog"]
            if folder_state(app, catalog) != event["data"]["state"]:
                fail("folder_configuration_changed")
            reconciler = Reconciler(app)
            if apply:
                reconciler.recover(catalog)
                remaining = reconciler.preview(catalog)
                authorized = event["data"]["reconciliation"]["actions"]
                if not remaining["safe"] or any(
                    a not in authorized
                    for a in remaining["actions"]
                    if a["kind"] != "unchanged"
                ):
                    fail("folder_recovery_requires_new_plan")
                reconciler.apply(
                    catalog,
                    max_removals=event["data"]["reconciliation"]["removal_budget"][
                        "removals"
                    ],
                )
            if reconciler.verify(catalog)["healthy"]:
                state = "verified"
        elif resolution is not None:
            if apply and expected_plan != plan_id:
                fail("recovery_requires_expected_plan")
            state = resolution
        if apply:
            event.pop("id")
            event["state"] = state
            if resolution:
                event["verification"] = "operator_attested"
                event["destination_identity"] = destination_identity
            record(store, profile, identifier, event)
        return {
            "event_id": identifier,
            "state": state,
            "complete": state in TERMINAL,
            "plan_id": plan_id,
            "recorded": apply,
            "requires_destination_inspection": op == "import",
            "destination_identity": destination_identity,
            "data": event["data"],
        }
