# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Bounded configuration transfer through existing owners; never executes media work."""

import copy
from contextlib import nullcontext

from .app import Application
from .domain import CatabolicError, name
from .layouts import Layouts
from .operations import Operations
from .outputs import Outputs
from .projections import Projections
from .rules import Rules
from .saved_queries import Queries
from .store import encode

FORMAT = "catabolic.program"
MAX_BYTES = 4 * 1024 * 1024
MAX_RECORDS = 256
KINDS = ("queries", "outputs", "operations", "rules", "layouts", "projections")


class _ImportStore:
    """Let configuration owners share the import's single rollback boundary."""

    def __init__(self, store):
        self._store = store

    def __getattr__(self, key):
        return getattr(self._store, key)

    def transaction(self):
        return nullcontext(self._store.db)


class _Preview(Exception):
    pass


class Programs:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def export(self, *, queries=(), rules=(), projections=()):
        records = {kind: {} for kind in KINDS}
        visiting = set()

        def add(kind, identifier):
            if identifier in records[kind]:
                return
            key = kind, identifier
            if key in visiting or len(visiting) >= 32:
                raise CatabolicError("program dependency cycle or depth exceeds 32")
            if sum(map(len, records.values())) + len(visiting) >= MAX_RECORDS:
                raise CatabolicError("program exceeds 256 definitions")
            visiting.add(key)
            if kind == "queries":
                row = Queries(self.store, self.profile).get(identifier)
                value = copy.deepcopy(row["definition"])
                for ref in value.get("queries", []):
                    add("queries", ref)
                if "selection" in value:
                    value["selection"].pop("profile", None)
                record = {"name": row["name"], "definition": value}
            elif kind == "outputs":
                row = Outputs(self.app).get_definition(identifier)
                record = {"name": row["name"], "definition": row["definition"]}
            elif kind == "operations":
                row = Operations(self.app).get(identifier)
                value = copy.deepcopy(row["definition"])
                if row["operation_kind"] == "render":
                    value = {
                        "kind": "render",
                        "operation": row["preset"],
                        "output_definition_id": row["output_definition_id"],
                        "options": {
                            k: v
                            for k, v in value.items()
                            if k not in ("version", "preset", "output_definition_id")
                        },
                    }
                    if value["output_definition_id"] is None:
                        # Pre-schema-9 recipes use the renderer's builtin output.
                        del value["output_definition_id"]
                if "output_definition_id" in value:
                    add("outputs", value["output_definition_id"])
                record = {"name": row["name"], "definition": value}
            elif kind == "rules":
                row = Rules(self.app).get(identifier)
                add("operations", row["recipe_id"])
                selection = copy.deepcopy(row["selection"])
                selection.pop("profile", None)
                if "query_id" in selection:
                    add("queries", selection["query_id"])
                record = {
                    "name": row["name"],
                    "operation_id": row["recipe_id"],
                    "location": row["location"],
                    "selection": selection,
                    "estimates": row["estimate_options"],
                    "required": row["required"],
                    "allow_derived": bool(row["allow_derived"]),
                }
            elif kind == "layouts":
                row = Layouts(self.app).get(identifier)
                value = copy.deepcopy(row["definition"])
                if "selection" in value:
                    value["selection"].pop("profile", None)
                    if "query_id" in value["selection"]:
                        add("queries", value["selection"]["query_id"])
                record = {"name": row["name"], "definition": value}
            else:
                row = Projections(self.app).get(identifier)
                if row["legacy"]:
                    raise CatabolicError(
                        "export requires an explicit projection binding"
                    )
                add("queries", row["query_id"])
                add("layouts", row["layout"])
                policy = row["rendition_policy"]
                if policy:
                    for field, target in (
                        ("definition_id", "outputs"),
                        ("rule_id", "rules"),
                    ):
                        if field in policy:
                            add(target, policy[field])
                record = {
                    "catalog": identifier,
                    "query_id": row["query_id"],
                    "layout": row["layout"],
                    "copies": row["copy_policy"],
                    "renditions": policy,
                }
            records[kind][identifier] = record
            visiting.remove(key)

        for kind, roots in (
            ("queries", queries),
            ("rules", rules),
            ("projections", projections),
        ):
            for identifier in roots:
                add(kind, identifier)
        bundle = {"format": FORMAT, "version": 1, **records}
        self.validate(bundle)
        return bundle

    @staticmethod
    def validate(bundle):
        if (
            not isinstance(bundle, dict)
            or set(bundle) != {"format", "version", *KINDS}
            or bundle["format"] != FORMAT
            or type(bundle["version"]) is not int
            or bundle["version"] != 1
        ):
            raise CatabolicError("expected a version 1 catabolic.program bundle")
        if len(encode(bundle).encode()) > MAX_BYTES:
            raise CatabolicError("program bundle exceeds 4 MiB")
        fields = {
            "queries": {"name", "definition"},
            "outputs": {"name", "definition"},
            "operations": {"name", "definition"},
            "layouts": {"name", "definition"},
            "rules": {
                "name",
                "operation_id",
                "location",
                "selection",
                "estimates",
                "required",
                "allow_derived",
            },
            "projections": {"catalog", "query_id", "layout", "copies", "renditions"},
        }
        count = 0
        for kind in KINDS:
            if not isinstance(bundle[kind], dict):
                raise CatabolicError("program sections must be objects")
            count += len(bundle[kind])
            for key, value in bundle[kind].items():
                if (
                    not isinstance(key, str)
                    or not key
                    or len(key) > 255
                    or not isinstance(value, dict)
                    or set(value) != fields[kind]
                ):
                    raise CatabolicError("invalid program " + kind + " record")
                if kind != "projections":
                    if not isinstance(value["name"], str):
                        raise CatabolicError("program names must be strings")
                    name(value["name"])
                if "definition" in value and not isinstance(value["definition"], dict):
                    raise CatabolicError("program definition must be an object")
                if kind == "rules" and (
                    type(value["required"]) is not bool
                    or type(value["allow_derived"]) is not bool
                    or not isinstance(value["selection"], dict)
                    or not isinstance(value["estimates"], dict)
                ):
                    raise CatabolicError("invalid program rule options")
        if not 1 <= count <= MAX_RECORDS:
            raise CatabolicError("program requires 1..256 definitions")

    def import_bundle(self, bundle, *, bindings=None, prefix="imported", dry_run=False):
        self.validate(bundle)
        self.app.require_recovered()
        if self.store.lock_fd is None or self.store.db.in_transaction:
            raise CatabolicError(
                "program import requires the writer lock outside a transaction"
            )
        if not isinstance(prefix, str):
            raise CatabolicError("import prefix must be a name")
        name(prefix)
        bindings = {} if bindings is None else bindings
        if not isinstance(bindings, dict) or set(bindings) - {
            "catalogs",
            "locations",
            "processors",
        }:
            raise CatabolicError("bindings supports catalogs, locations and processors")
        for mapping in bindings.values():
            if not isinstance(mapping, dict):
                raise CatabolicError("binding maps must be objects")
            for key, value in mapping.items():
                if not isinstance(key, str) or not isinstance(value, str):
                    raise CatabolicError("binding names must be strings")
                name(key)
                name(value)
        app = Application(_ImportStore(self.store), self.profile)
        result = {kind: {} for kind in KINDS}
        visiting = set()
        destination_catalogs = set()

        def bound(kind, identifier):
            if not isinstance(identifier, str) or identifier not in bindings.get(
                kind, {}
            ):
                raise CatabolicError(
                    f"explicit {kind} binding required for {identifier!r}"
                )
            return bindings[kind][identifier]

        def selection(value):
            value = copy.deepcopy(value)
            value["profile"] = self.profile
            if "query_id" in value:
                value["query_id"] = put("queries", value["query_id"])
            return value

        def put(kind, identifier):
            if not isinstance(identifier, str) or identifier not in bundle[kind]:
                raise CatabolicError("missing program " + kind + " reference")
            if identifier in result[kind]:
                return result[kind][identifier]
            key = kind, identifier
            if key in visiting or len(visiting) >= 32:
                raise CatabolicError("program dependency cycle or depth exceeds 32")
            visiting.add(key)
            row = copy.deepcopy(bundle[kind][identifier])
            local_name = prefix + "." + row["name"] if "name" in row else None
            if kind == "queries":
                value = row["definition"]
                if "queries" in value:
                    if not isinstance(value["queries"], list):
                        raise CatabolicError("query references must be an array")
                    value["queries"] = [put("queries", ref) for ref in value["queries"]]
                if "selection" in value:
                    if not isinstance(value["selection"], dict):
                        raise CatabolicError("query selection must be an object")
                    value["selection"] = selection(value["selection"])
                local = Queries(app.store, self.profile).put(local_name, value)["id"]
            elif kind == "outputs":
                local = Outputs(app).define(local_name, row["definition"])["id"]
            elif kind == "operations":
                value = row["definition"]
                if "output_definition_id" in value:
                    value["output_definition_id"] = put(
                        "outputs", value["output_definition_id"]
                    )
                if value.get("kind") == "external":
                    value["operation"] = bound("processors", value.get("operation"))
                local = Operations(app).put(local_name, value)["id"]
            elif kind == "rules":
                existing = {
                    r["id"]
                    for r in app.store.rows(
                        "SELECT id FROM processing_rules WHERE profile=? AND name=?",
                        (self.profile, local_name),
                    )
                }
                local = Rules(app).put(
                    local_name,
                    put("operations", row["operation_id"]),
                    bound("locations", row["location"])
                    if row["location"] is not None
                    else None,
                    selection(row["selection"]),
                    estimates=row["estimates"],
                    required=row["required"],
                    allow_derived=row["allow_derived"],
                )["id"]
                if existing and local not in existing:
                    raise CatabolicError(
                        "import would replace an existing rule; choose another prefix"
                    )
                if local not in existing:
                    Rules(app).enable(local, False)
            elif kind == "layouts":
                value = row["definition"]
                if "selection" in value:
                    if not isinstance(value["selection"], dict):
                        raise CatabolicError("layout selection must be an object")
                    value["selection"] = selection(value["selection"])
                old = Layouts(app)._read("layout:" + local_name)
                if old is not None and old != value:
                    raise CatabolicError(
                        "import would replace an existing layout; choose another prefix"
                    )
                local = Layouts(app).put(local_name, value)["name"]
            else:
                local = bound("catalogs", row["catalog"])
                if local in destination_catalogs:
                    raise CatabolicError(
                        "multiple projections cannot share an import destination"
                    )
                destination_catalogs.add(local)
                query = put("queries", row["query_id"])
                layout = put("layouts", row["layout"])
                policy = row["renditions"]
                if policy is not None:
                    if not isinstance(policy, dict):
                        raise CatabolicError("rendition policy must be an object")
                    for field, target in (
                        ("definition_id", "outputs"),
                        ("rule_id", "rules"),
                    ):
                        if field in policy:
                            policy[field] = put(target, policy[field])
                before = Projections(app).get(local)
                expected = {
                    "query_id": query,
                    "layout": layout,
                    "copy_policy": row["copies"],
                    "rendition_policy": policy,
                }
                if not before["legacy"] and any(
                    before[k] != v for k, v in expected.items()
                ):
                    raise CatabolicError("import would replace an existing projection")
                if before["legacy"] and (
                    before["applied_layout"]
                    or before["copy_policy"] is not None
                    or before["rendition_policy"] is not None
                    or before["refresh"]
                ):
                    raise CatabolicError(
                        "import requires an unconfigured destination catalog"
                    )
                Projections(app).put(
                    local, query, layout, copies=row["copies"], renditions=policy
                )
            result[kind][identifier] = local
            visiting.remove(key)
            return local

        try:
            with self.store.transaction():
                for kind in KINDS:
                    for identifier in bundle[kind]:
                        put(kind, identifier)
                if dry_run:
                    raise _Preview()
        except _Preview:
            pass
        return {
            "format": FORMAT,
            "version": 1,
            "complete": True,
            "applied": not dry_run,
            "dry_run": dry_run,
            "references": result,
            "preview_ids_ephemeral": dry_run,
        }
