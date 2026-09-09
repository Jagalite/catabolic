# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Explicit setup; remote reads/mutations occur outside every Store lifetime."""

import json
import time

from .app import Application
from .consumer_adapters import (
    ConsumerError,
    adapter,
    consumer_credential_ref,
    endpoint,
    path,
    text,
    translate,
    validate_remote,
    within,
)
from .consumers import binding, connection, group_id
from .store import Store, encode


def get_connection(database, profile, identifier):
    with Store(database) as store:
        return connection(Application(store, profile), identifier)


def put_connection(
    database,
    profile,
    identifier,
    application,
    address,
    credential_env,
    *,
    apply=False,
    repair=False,
):
    text(identifier, 255)
    if application not in ("plex", "jellyfin"):
        raise ConsumerError("unsupported")
    if (
        application != "plex"
        and isinstance(credential_env, str)
        and credential_env.startswith("file:")
    ):
        raise ConsumerError("plex_credential_requires_plex_connection")
    value = {
        "id": identifier,
        "profile": profile,
        "application": application,
        "endpoint": endpoint(address),
        "credential_env": consumer_credential_ref(credential_env),
    }
    identity = adapter(value).inspect()
    value.update(server_id=identity["server_id"], evidence=encode(identity))
    if apply:
        with Store(database, writable=True) as store:
            old = store.rows(
                "SELECT * FROM consumer_connections WHERE profile=? AND id=?",
                (profile, identifier),
            )
            if old and any(
                old[0][k] != value[k] for k in ("application", "endpoint", "server_id")
            ):
                raise ConsumerError("connection_identity_changed_use_new_connection")
            if old and old[0]["credential_env"] != credential_env and not repair:
                raise ConsumerError("explicit_repair_required")
            with store.transaction() as db:
                db.execute(
                    """INSERT INTO consumer_connections(profile,id,application,endpoint,credential_env,server_id,evidence)
                VALUES (:profile,:id,:application,:endpoint,:credential_env,:server_id,:evidence)
                ON CONFLICT(profile,id) DO UPDATE SET credential_env=excluded.credential_env,evidence=excluded.evidence,
                revision=consumer_connections.revision+CASE WHEN consumer_connections.credential_env!=excluded.credential_env THEN 1 ELSE 0 END""",
                    value,
                )
    return {
        "connection": {**value, "evidence": identity},
        "applied": apply,
        "complete": True,
    }


def discover(database, profile, identifier, kind=None):
    c = get_connection(database, profile, identifier)
    a, identity = validate_remote(c)
    result = {"identity": identity, "libraries": a.libraries(), "complete": True}
    if kind:
        try:
            result["creation_choices"] = a.choices(kind)
        except ConsumerError as exc:
            result["creation_choices"] = {"error": exc.result(), "complete": False}
    return result


def bind(
    database,
    profile,
    identifier,
    connection_id,
    catalog,
    subtree,
    remote_root,
    library_id,
    kind,
    *,
    apply=False,
    automatic=False,
    initial_scan=False,
    debounce=0,
    rebind=False,
    origin="adopted",
):
    text(identifier, 255)
    subtree, remote_root = path(subtree, relative=True), path(remote_root)
    if type(debounce) is not int or not 0 <= debounce <= 3600:
        raise ConsumerError("invalid_configuration")
    c = get_connection(database, profile, connection_id)
    a, _ = validate_remote(c)
    libraries = a.libraries()
    # Stable ID is mandatory. Duplicate names never participate in selection.
    found = [r for r in libraries if r["id"] == library_id]
    if len(found) != 1 or found[0]["type"] != kind:
        raise ConsumerError("library_mismatch")
    library = found[0]
    if sum(within(remote_root, root) for root in library["roots"]) != 1:
        raise ConsumerError("library_root_mismatch")
    with Store(database, writable=apply) as store:
        app = Application(store, profile)
        if connection(app, connection_id)["revision"] != c["revision"]:
            raise ConsumerError("connection_changed")
        local = app.binding("output", catalog)
        group = group_id(profile, c["application"], c["server_id"], library)
        existing = store.rows(
            "SELECT * FROM consumer_bindings WHERE profile=? AND id=?",
            (profile, identifier),
        )
        value = {
            "profile": profile,
            "id": identifier,
            "connection_id": connection_id,
            "catalog": catalog,
            "subtree": subtree,
            "remote_root": remote_root,
            "local_binding": encode(local),
            "library": encode(library),
            "group_id": group,
            "origin": origin,
            "automatic": int(automatic),
            "debounce": debounce,
        }
        identity_fields = (
            "connection_id",
            "catalog",
            "subtree",
            "remote_root",
            "local_binding",
            "group_id",
        )
        changed = existing and any(existing[0][k] != value[k] for k in identity_fields)
        if existing:
            previous_library = json.loads(existing[0]["library"])
            changed = changed or any(
                previous_library.get(k) != library.get(k)
                for k in ("uuid", "roots", "type", "created_at")
            )
        if existing and existing[0]["group_id"] == group:
            value["origin"] = existing[0]["origin"]
        if changed and not rebind:
            raise ConsumerError("explicit_rebind_required")
        for other in store.rows(
            "SELECT * FROM consumer_bindings WHERE profile=? AND group_id=? AND id!=? AND enabled=1",
            (profile, group, identifier),
        ):
            if within(remote_root, other["remote_root"]) or within(
                other["remote_root"], remote_root
            ):
                raise ConsumerError("overlapping_binding_roots")
        if apply:
            with store.transaction() as db:
                db.execute(
                    "INSERT INTO consumer_deliveries(profile,id) VALUES (?,?) ON CONFLICT DO NOTHING",
                    (profile, group),
                )
                if not existing:
                    db.execute(
                        """INSERT INTO consumer_bindings(profile,id,connection_id,catalog,subtree,remote_root,local_binding,library,group_id,origin,automatic,debounce)
                    VALUES (:profile,:id,:connection_id,:catalog,:subtree,:remote_root,:local_binding,:library,:group_id,:origin,:automatic,:debounce)""",
                        value,
                    )
                elif (
                    changed
                    or not existing[0]["enabled"]
                    or existing[0]["automatic"] != int(automatic)
                    or existing[0]["debounce"] != debounce
                ):
                    db.execute(
                        """UPDATE consumer_bindings SET connection_id=:connection_id,catalog=:catalog,subtree=:subtree,remote_root=:remote_root,local_binding=:local_binding,library=:library,group_id=:group_id,origin=:origin,automatic=:automatic,debounce=:debounce,enabled=1,revision=revision+1,error=NULL,indexing=NULL WHERE profile=:profile AND id=:id""",
                        value,
                    )
                    if changed:
                        db.execute(
                            "UPDATE consumer_bindings SET generation=0,verified=0,acknowledged=0 WHERE profile=? AND id=?",
                            (profile, identifier),
                        )
                # Repeated setup must not request repeated initial scans.
                if initial_scan and (not existing or changed):
                    db.execute(
                        "UPDATE consumer_bindings SET generation=generation+1 WHERE profile=? AND id=?",
                        (profile, identifier),
                    )
            if initial_scan:
                from .consumers import published
                from .reconcile import Reconciler

                published(app, Reconciler(app).verify(catalog), value)
    return {
        "binding": {**value, "library": library, "local_binding": local},
        "applied": apply,
        "path_evidence": "declared mapping and server-reported root; remote symlink target readability unverified",
        "complete": True,
    }


def creation(
    database, profile, identifier, connection_id, spec, *, apply=False, reconcile=False
):
    text(identifier, 255)
    required = {"name", "type", "root", "scanner", "agent", "language"}
    if not isinstance(spec, dict) or set(spec) != required:
        raise ConsumerError("invalid_creation_spec")
    spec = {k: text(v) for k, v in spec.items()}
    spec["root"] = path(spec["root"])
    c = get_connection(database, profile, connection_id)
    a, _ = validate_remote(c)
    if not a.capabilities.create_library:
        raise ConsumerError("unsupported")
    libraries = a.libraries()
    with Store(database) as store:
        rows = store.rows(
            "SELECT * FROM consumer_creations WHERE profile=? AND id=?",
            (profile, identifier),
        )
    if rows:
        intent = rows[0]
        if (
            intent["spec"] != encode(spec)
            or intent["connection_id"] != connection_id
            or intent["connection_revision"] != c["revision"]
        ):
            raise ConsumerError("creation_intent_mismatch")
        if intent["state"] == "complete":
            library = json.loads(intent["library"])
            validate_remote(c, library)
            return {
                "library": library,
                "origin": "created",
                "reused": True,
                "complete": True,
            }
        candidates = [
            r
            for r in libraries
            if r["id"] not in json.loads(intent["before_ids"])
            and r["name"] == spec["name"]
            and r["type"] == spec["type"]
            and r["roots"] == [spec["root"]]
            and r.get("scanner") == spec["scanner"]
            and r.get("agent") == spec["agent"]
        ]
        if len(candidates) != 1:
            return {
                "complete": False,
                "state": "uncertain",
                "error": ConsumerError("creation_uncertain").result(),
                "candidates": len(candidates),
                "retry_post": False,
            }
        if not (apply or reconcile):
            return {
                "complete": True,
                "applied": False,
                "would_reconcile": candidates[0],
            }
        with Store(database, writable=True) as store, store.transaction() as db:
            db.execute(
                "UPDATE consumer_creations SET state='complete',library=?,error=NULL WHERE profile=? AND id=?",
                (encode(candidates[0]), profile, identifier),
            )
        return {
            "library": candidates[0],
            "origin": "created_reconciled",
            "complete": True,
        }
    choices = a.choices(spec["type"])
    if (
        spec["scanner"] not in choices["scanners"]
        or spec["agent"] not in choices["agents"]
    ):
        raise ConsumerError("unsupported_creation_settings")
    if any(r["name"] == spec["name"] and spec["root"] in r["roots"] for r in libraries):
        raise ConsumerError("existing_library_requires_explicit_binding")
    if not apply:
        return {
            "spec": spec,
            "connection_id": connection_id,
            "applied": False,
            "complete": True,
        }
    with Store(database, writable=True) as store, store.transaction() as db:
        if store.rows(
            "SELECT 1 FROM consumer_creations WHERE profile=? AND connection_id=? AND spec=?",
            (profile, connection_id, encode(spec)),
        ):
            raise ConsumerError("creation_intent_already_exists")
        if (
            connection(Application(store, profile), connection_id)["revision"]
            != c["revision"]
        ):
            raise ConsumerError("connection_changed")
        db.execute(
            "INSERT INTO consumer_creations(profile,id,connection_id,connection_revision,spec,before_ids,state) VALUES (?,?,?,?,?,?,'uncertain')",
            (
                profile,
                identifier,
                connection_id,
                c["revision"],
                encode(spec),
                encode([r["id"] for r in libraries]),
            ),
        )
    try:
        a.create(spec)
    except ConsumerError:
        # A timeout may mean the POST succeeded. Never replay it automatically.
        pass
    return creation(database, profile, identifier, connection_id, spec, reconcile=True)


def verify_indexing(database, profile, identifier, limit=100):
    from .consumers import local_valid

    if not 1 <= limit <= 1000:
        raise ConsumerError("invalid_configuration")
    with Store(database) as store:
        app = Application(store, profile)
        b = binding(app, identifier)
        if not b["enabled"]:
            raise ConsumerError("binding_disabled")
        local_valid(app, [b])
        c = connection(app, b["connection_id"])
        paths = [
            r["path"]
            for r in store.rows(
                "SELECT path FROM mappings WHERE catalog=? AND active=1 AND (?='' OR path=? OR substr(path,1,length(?)+1)=?||'/') ORDER BY path LIMIT ?",
                (
                    b["catalog"],
                    b["subtree"],
                    b["subtree"],
                    b["subtree"],
                    b["subtree"],
                    limit + 1,
                ),
            )
            if within(r["path"], b["subtree"])
        ]
        if len(paths) > limit:
            raise ConsumerError("response_limit")
    a, library = validate_remote(c, json.loads(b["library"]))
    result = a.indexed(library, [translate(b, p) for p in paths], limit)
    with Store(database, writable=True) as store, store.transaction() as db:
        current = binding(Application(store, profile), identifier)
        if (
            current["revision"] != b["revision"]
            or current["generation"] != b["generation"]
        ):
            raise ConsumerError("binding_changed")
        db.execute(
            "UPDATE consumer_bindings SET indexing=? WHERE profile=? AND id=?",
            (
                encode(
                    {
                        **result,
                        "generation_observed": b["generation"],
                        "observed_at": time.time(),
                    }
                ),
                profile,
                identifier,
            ),
        )
    return result
