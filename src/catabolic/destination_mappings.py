# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Complete saved-query rows projected through capability-declaring adapters.

Definitions are portable JSON; reviewed plans pin the definition, query result,
connection, remote identity, and ownership ledger. Queries never perform writes.
"""

import hashlib
import json
from uuid import uuid4

from .consumer_adapters import ConsumerError, text, validate_remote
from .consumer_setup import get_connection
from .metadata_publication import serialize_writes
from .saved_queries import Queries
from .store import Store, encode


def fail(message):
    raise ConsumerError("mapping_" + message)


def digest(value):
    return hashlib.sha256(encode(value).encode()).hexdigest()


def expression(spec, row):
    if not isinstance(spec, dict) or set(spec) - {
        "column",
        "constant",
        "lookup",
        "json",
    }:
        fail("invalid_expression")
    if ("column" in spec) == ("constant" in spec):
        fail("expression_requires_column_or_constant")
    if "column" in spec:
        if not isinstance(spec["column"], str) or spec["column"] not in row:
            fail("missing_column")
        value = row[spec["column"]]
    else:
        value = spec["constant"]
    if "json" in spec:
        if spec["json"] is not True or not isinstance(value, str):
            fail("invalid_json_expression")
        try:
            value = json.loads(value)
        except ValueError:
            fail("invalid_json_value")
    if "lookup" in spec:
        lookup = spec["lookup"]
        if (
            not isinstance(lookup, dict)
            or not isinstance(value, str)
            or value not in lookup
        ):
            fail("unmapped_lookup_value")
        value = lookup[value]
    return value


def evaluate(database, profile, definition, adapter):
    capabilities = adapter.capabilities
    if (
        not isinstance(definition, dict)
        or set(definition)
        != {"version", "id", "query", "destination", "key", "target", "fields"}
        or type(definition["version"]) is not int
        or definition["version"] != 1
    ):
        fail("invalid_definition")
    text(definition["id"], 255)
    destination = definition["destination"]
    if (
        not isinstance(destination, dict)
        or set(destination) - {"connection", "library", "collections"}
        or not {"connection", "library"} <= set(destination)
    ):
        fail("invalid_destination")
    for k in ("connection", "library"):
        text(destination[k], 255)
    fields = definition["fields"]
    target = definition["target"]
    if not isinstance(target, dict) or set(target) != {"id", "path"}:
        fail("target_requires_id_and_path")
    if not isinstance(fields, dict) or not fields or set(fields) - set(capabilities):
        fail("unsupported_fields")
    with Store(database) as store:
        queries = Queries(store, profile)
        if queries.get(definition["query"])["definition"]["mode"] != "rows":
            fail("requires_rows_query")
        result = queries.run(definition["query"], 1000)
        database_id = store.database_id
    if result.get("complete") is not True or result.get("truncated") is not False:
        fail("incomplete_query")
    columns = result["columns"]
    if len(set(columns)) != len(columns):
        fail("duplicate_columns")
    grouped = {}
    targets = {}
    for values in result["rows"]:
        row = dict(zip(columns, values, strict=True))
        key = text(expression(definition["key"], row), 255)
        identity = {k: text(expression(v, row)) for k, v in target.items()}
        if identity["id"] in targets and targets[identity["id"]] != key:
            fail("duplicate_target")
        targets[identity["id"]] = key
        entry = grouped.setdefault(key, {"key": key, "target": identity, "values": {}})
        if entry["target"] != identity:
            fail("key_has_multiple_targets")
        for field, spec in fields.items():
            value = expression(spec, row)
            if capabilities[field] == "set":
                if isinstance(value, str):
                    value = [value]
                if not isinstance(value, list) or len(value) > 100:
                    fail("invalid_set")
                value = sorted({text(v, 255) for v in value})
                entry["values"][field] = sorted(
                    set(entry["values"].get(field, [])) | set(value)
                )
                if len(entry["values"][field]) > 100:
                    fail("set_too_large")
            else:
                value = adapter.validate_value(field, value)
                if field in entry["values"] and entry["values"][field] != value:
                    fail("conflicting_scalar_rows")
                entry["values"][field] = value
    if len(grouped) > 100:
        fail("too_many_targets")
    return {"database_id": database_id, "rows": [grouped[k] for k in sorted(grouped)]}


def events(database, profile):
    with Store(database) as store:
        return [
            dict(id=r["id"], **json.loads(r["subject"]))
            for r in store.rows(
                "SELECT id,subject FROM consumer_events WHERE profile=? AND event='mapping_publish' ORDER BY rowid",
                (profile,),
            )
        ]


def save_event(database, profile, identifier, payload, *, create=False):
    with Store(database, writable=True) as store:
        write_event(store, profile, identifier, payload, create=create)


def write_event(store, profile, identifier, payload, *, create=False):
    with store.transaction() as db:
        if create:
            db.execute(
                "INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,?,'mapping_publish','info',?)",
                (identifier, profile, encode(payload)),
            )
        else:
            db.execute(
                "UPDATE consumer_events SET subject=?,severity=? WHERE id=? AND profile=? AND event='mapping_publish'",
                (
                    encode(payload),
                    "info"
                    if payload["state"] in ("verified", "submitted", "not_applied")
                    else "warning",
                    identifier,
                    profile,
                ),
            )


def ownership(history, scope):
    ledger = {}
    for event in history:
        if event["scope"] != scope:
            continue
        if event["state"] not in ("verified", "not_applied"):
            fail("unresolved_write_recover_" + event["id"])
        if event["state"] == "verified":
            for field, value in event["owned"].items():
                ledger[(event["target"]["id"], field)] = {
                    "owner": event["owner"],
                    "key": event["key"],
                    "identity": event["before"]["identity"],
                    **value,
                }
    return ledger


def prepare(database, profile, definition):
    from .mapping_adapters import destination_adapter

    if not isinstance(definition, dict) or not isinstance(
        definition.get("destination"), dict
    ):
        fail("invalid_definition")
    c = get_connection(database, profile, definition["destination"].get("connection"))
    a, _ = validate_remote(c)
    adapter = destination_adapter(
        c["application"], a, definition["destination"].get("collections")
    )
    local = evaluate(database, profile, definition, adapter)
    libraries = [
        r for r in a.libraries() if r["id"] == definition["destination"]["library"]
    ]
    if len(libraries) != 1:
        fail("unknown_library")
    library = libraries[0]
    scope = [c["application"], c["server_id"], library["uuid"]]
    ledger = ownership(events(database, profile), scope)
    rows = []
    removals = 0
    planned_bytes = 0
    for item in local["rows"]:
        before = adapter.read(library, item["target"], item["values"])
        after = {}
        owned = {}
        for field, wanted in item["values"].items():
            current = before["values"][field]
            previous = ledger.get((item["target"]["id"], field))
            if previous and (
                previous["owner"] != definition["id"] or previous["key"] != item["key"]
            ):
                fail("field_owned_by_other_mapping")
            if previous and previous["identity"] != before["identity"]:
                fail("owned_target_identity_changed")
            if adapter.capabilities[field] == "set":
                old = set(previous["contribution"]) if previous else set()
                if not old <= set(current):
                    fail("owned_values_changed_remotely")
                manual = set(current) - old
                after[field] = sorted(manual | set(wanted))
                contribution = sorted(set(wanted) - manual)
                removals += len(set(current) - set(after[field]))
            else:
                if previous and current != previous["last"]:
                    fail("owned_value_changed_remotely")
                after[field] = wanted
                contribution = wanted
            owned[field] = {"last": after[field], "contribution": contribution}
        row = {**item, "before": before, "after": after, "owned": owned}
        adapter.validate(library, row)
        planned_bytes += len(encode(row).encode())
        if planned_bytes > 4 * 1024 * 1024:
            fail("plan_too_large")
        rows.append(row)
    plan = {
        "definition": definition,
        "local": local,
        "connection": c,
        "library": library,
        "scope": scope,
        "rows": rows,
        "removals": removals,
        "ownership": sorted((list(k), v) for k, v in ledger.items()),
    }
    if len(encode(plan).encode()) > 4 * 1024 * 1024:
        fail("plan_too_large")
    return plan, adapter


@serialize_writes
def run(
    database, profile, definition, *, apply=False, expected_plan=None, max_removals=0
):
    if isinstance(definition, dict) and definition.get("version") == 2:
        from .publication_mappings import run as publish

        return publish(
            database,
            profile,
            definition,
            apply=apply,
            expected_plan=expected_plan,
            max_removals=max_removals,
        )
    if type(max_removals) is not int or max_removals < 0:
        fail("invalid_removal_budget")
    if apply and not expected_plan:
        fail("apply_requires_expected_plan")
    plan, adapter = prepare(database, profile, definition)
    plan_id = digest(plan)
    result = {
        "plan_id": plan_id,
        "mapping": definition["id"],
        "removals": plan["removals"],
        "rows": [
            {k: r[k] for k in ("key", "target", "before", "after")}
            for r in plan["rows"]
        ],
        "applied": False,
        "complete": True,
        "results": [],
    }
    if not apply:
        return result
    if plan_id != expected_plan:
        fail("stale_plan_preview_again")
    if plan["removals"] > max_removals:
        fail("removal_budget_exceeded")
    for row in plan["rows"]:
        # No Store transaction is held across remote calls. Recheck local inputs
        # and destination before each write, including after earlier batch writes.
        if evaluate(database, profile, definition, adapter) != plan["local"]:
            fail("query_changed_preview_again")
        if (
            get_connection(database, profile, definition["destination"]["connection"])
            != plan["connection"]
        ):
            fail("connection_changed")
        validate_remote(plan["connection"], plan["library"])
        if adapter.read(plan["library"], row["target"], row["values"]) != row["before"]:
            fail("remote_changed_preview_again")
        identifier = str(uuid4())
        payload = {
            "owner": definition["id"],
            "query_id": definition["query"],
            "definition_digest": digest(definition),
            "key": row["key"],
            "scope": plan["scope"],
            "collections": definition["destination"].get("collections"),
            "connection": plan["connection"],
            "library": plan["library"],
            "target": row["target"],
            "before": row["before"],
            "after": row["after"],
            "owned": row["owned"],
            "state": "started",
            "plan_id": plan_id,
        }
        save_event(database, profile, identifier, payload, create=True)
        try:
            if row["before"]["values"] != row["after"]:
                adapter.write(plan["library"], row)
            validate_remote(plan["connection"], plan["library"])
            observed = adapter.read(plan["library"], row["target"], row["values"])
            if not adapter.matches(observed, row["before"], row["after"]):
                fail("verification_failed")
            payload["state"] = "verified"
            save_event(database, profile, identifier, payload)
            result["results"].append(
                {"key": row["key"], "event_id": identifier, "state": "verified"}
            )
        except Exception as exc:
            payload["state"] = "uncertain"
            save_event(database, profile, identifier, payload)
            result.update(
                event_id=identifier,
                state="uncertain",
                complete=False,
                safe_to_retry=False,
                error=getattr(exc, "code", "mapping_write_or_verification_failed"),
            )
            return result
    result["applied"] = True
    return result


@serialize_writes
def recover(
    database, profile, identifier, *, apply=False, resolution=None, expected_plan=None
):
    """Read back an interrupted attempt. Never replay or undo a remote write."""
    from .mapping_adapters import destination_adapter

    matches = [e for e in events(database, profile) if e["id"] == identifier]
    if len(matches) != 1:
        fail("unknown_event")
    event = matches[0]
    if event.get("version") == 2:
        from .publication_mappings import recover as recover_publication

        return recover_publication(
            database,
            profile,
            identifier,
            apply=apply,
            resolution=resolution,
            expected_plan=expected_plan,
        )
    if resolution is not None or expected_plan is not None:
        fail("unsupported_recovery_options")
    if event["state"] in ("verified", "not_applied"):
        return {"event_id": identifier, "state": event["state"], "complete": True}
    c = get_connection(database, profile, event["connection"]["id"])
    if c != event["connection"]:
        fail("connection_changed")
    a, _ = validate_remote(c, event["library"])
    adapter = destination_adapter(c["application"], a, event.get("collections"))
    observed = adapter.read(event["library"], event["target"], event["after"])
    if adapter.matches(observed, event["before"], event["after"]):
        state = "verified"
    elif observed == event["before"]:
        state = "not_applied"
    else:
        state = "uncertain"
    if apply:
        payload = {k: v for k, v in event.items() if k != "id"}
        payload["state"] = state
        save_event(database, profile, identifier, payload)
    return {
        "event_id": identifier,
        "state": state,
        "recorded": apply,
        "complete": state != "uncertain",
        "safe_to_retry": state == "not_applied",
    }
