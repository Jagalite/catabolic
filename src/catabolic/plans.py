# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Typed references to existing engines, conservative coverage and comparisons."""

import hashlib
from typing import Annotated, Literal

from pydantic import Field, TypeAdapter

from .domain import CatabolicError
from .evaluation import EvaluationSession
from .fallback_models import Model
from .fallback_policies import Policies
from .fallback_projection import FallbackProjection, binding
from .fallback_resolution import Resolver
from .layouts import Layouts
from .saved_queries import Queries
from .store import encode


class QueryPlan(Model):
    kind: Literal["query"]
    query_id: str


class FallbackPlan(Model):
    kind: Literal["fallback"]
    membership_query_id: str
    policy_id: str
    role: str = "primary"
    part: int | None = None
    variant: str = ""


class ProjectionPlan(Model):
    kind: Literal["projection"]
    catalog: str
    binding_digest: str


Plan = Annotated[QueryPlan | FallbackPlan | ProjectionPlan, Field(discriminator="kind")]
PLAN = TypeAdapter(Plan)


def projection_digest(app, catalog):
    config = binding(app.store, app.profile, catalog)
    if not config:
        raise CatabolicError(
            "watcher projection requires an authoritative fallback binding"
        )
    return hashlib.sha256(
        encode(
            {
                "query_id": config["query_id"],
                "policy_id": config["policy_id"],
                "layout": Layouts(app).get(config["layout"]),
                "output": app.store.rows(
                    "SELECT * FROM bindings WHERE profile=? AND kind='output' AND owner=?",
                    (app.profile, catalog),
                ),
            }
        ).encode()
    ).hexdigest()


def describe(app, reference):
    plan = PLAN.validate_python(reference)
    queries, policies = set(), set()
    pending = []
    if isinstance(plan, QueryPlan):
        pending.append(plan.query_id)
    else:
        if isinstance(plan, ProjectionPlan):
            if projection_digest(app, plan.catalog) != plan.binding_digest:
                raise CatabolicError("stale_watcher_projection_binding")
            config = binding(app.store, app.profile, plan.catalog)
            membership, policy = config["query_id"], config["policy_id"]
        else:
            membership, policy = plan.membership_query_id, plan.policy_id
        if (
            Queries(app.store, app.profile).get(membership)["definition"]["mode"]
            != "selection"
        ):
            raise CatabolicError("logical plan requires typed membership")
        pending.append(membership)
        policies.add(policy)
        pending.extend(
            t["query_id"]
            for t in Policies(app.store, app.profile).get(policy)["definition"][
                "fallbacks"
            ]
        )
        package = (
            Policies(app.store, app.profile).get(policy)["definition"].get("package")
        )
        if package:
            pending.extend(
                t["query_id"] for r in package["requirements"] for t in r["fallbacks"]
            )
    while pending:
        ref = pending.pop()
        if ref in queries:
            continue
        if len(queries) >= 64:
            raise CatabolicError("plan_dependency_budget")
        queries.add(ref)
        pending.extend(
            Queries(app.store, app.profile).get(ref)["definition"].get("queries", [])
        )
    return {
        "plan": plan.model_dump(),
        "queries": sorted(queries),
        "policies": sorted(policies),
        "coverage": "all_bound_profile_sources",
        "dependency_precision": "conservative",
        "catalog_facts": ["all"],
        "periodic_reevaluation_required": True,
    }


def observation_requirements(app, reference):
    describe(app, reference)
    return [
        {"source": r["owner"], "kind": "full_inventory", "scope": "", "exclusions": []}
        for r in app.store.rows(
            "SELECT owner FROM bindings WHERE profile=? AND kind='source' ORDER BY owner",
            (app.profile,),
        )
    ]


def evaluate(app, reference, *, session=None, previous=None, context_name=""):
    if session is not None and session.access is not None:
        raise CatabolicError(
            "plan orchestration currently requires local-owner authority"
        )
    description = describe(app, reference)
    plan = PLAN.validate_python(reference)
    session = session or EvaluationSession(app.store, app.profile, max_ids=100000)
    if isinstance(plan, ProjectionPlan):
        prepared = FallbackProjection(app).plan(plan.catalog, session=session)
        return {
            "kind": "projection",
            "complete": all(d["state"] != "blocked" for d in prepared["desired"]),
            "value": prepared,
            "description": description,
        }
    if isinstance(plan, FallbackPlan):
        entity, ids, report = session.select(plan.membership_query_id)
        if entity != "item_id":
            raise CatabolicError("fallback membership requires item IDs")
        entries = [
            dict(item_id=i, role=plan.role, part=plan.part, variant=plan.variant)
            for i in sorted(ids)
        ]
        value = (
            Resolver(app, access=session.access, session=session).resolve(
                entries,
                plan.policy_id,
                catalog=context_name,
                previous={
                    d["entry_id"]: {
                        **d,
                        "tier": d.get("selected_tier"),
                        "evidence": encode(d.get("evidence", {})),
                    }
                    for d in (previous or {}).get("value", [])
                    if d.get("policy_id") == plan.policy_id
                },
            )
            if entries
            else {"decisions": [], "complete": True}
        )
        return {
            "kind": "fallback",
            "complete": all(
                d["state"] != "blocked" for d in value.get("decisions", [])
            ),
            "value": value,
            "description": description,
        }
    query = Queries(app.store, app.profile).get(plan.query_id)["definition"]
    if query["mode"] == "selection":
        entity, ids, report = session.select(plan.query_id)
        component_evidence = None
        if entity in ("component_id", "occurrence_id"):
            from .component_sql import OCCURRENCES_SQL

            component_evidence = session.rows(
                "SELECT * FROM ("
                + OCCURRENCES_SQL
                + ") WHERE profile=? AND "
                + entity
                + " IN (SELECT value FROM json_each(?)) ORDER BY occurrence_id LIMIT 100001",
                (app.profile, encode(sorted(ids))),
            )
            if len(component_evidence) > 100000:
                raise CatabolicError("component_selection_expansion_budget")
            session.account(component_evidence)
        return {
            "kind": "selection",
            "complete": True,
            "value": {
                "entity": entity,
                "ids": sorted(ids),
                **(
                    {"component_evidence": component_evidence}
                    if component_evidence is not None
                    else {}
                ),
            },
            "description": description,
        }
    if session.access is not None:
        raise CatabolicError("report plans currently require local-owner execution")
    session.check(app.store, app.profile)
    value = Queries(app.store, app.profile).run(
        plan.query_id, limit=1000, session=session
    )
    session.check(app.store, app.profile)
    return {
        "kind": query["mode"],
        "complete": value.get("complete", not value.get("errors"))
        and not value.get("truncated", False),
        "value": value,
        "description": description,
    }


def semantic(result):
    kind, value = result["kind"], result["value"]
    if kind == "projection":
        value = {
            "desired": [
                {
                    k: d.get(k)
                    for k in (
                        "entry_id",
                        "file_id",
                        "revision",
                        "path",
                        "state",
                        "policy_id",
                        "selected_tier",
                        "rendition_id",
                    )
                }
                for d in value["desired"]
            ],
            "removed": value["removed"],
            "safe": value["safe"],
        }
    elif kind == "fallback":
        value = [
            {
                k: d.get(k)
                for k in (
                    "entry_id",
                    "file_id",
                    "revision",
                    "state",
                    "policy_id",
                    "selected_tier",
                    "rendition_id",
                )
            }
            for d in value.get("decisions", [])
        ]
    elif kind == "rows":
        value = {k: value.get(k) for k in ("columns", "rows")}
    elif kind == "document":
        value = value.get("data")
    if kind in ("fallback", "projection"):
        originals = result["value"].get(
            "decisions" if kind == "fallback" else "desired", []
        )
        selected = value if kind == "fallback" else value["desired"]
        for normalized, decision in zip(selected, originals, strict=True):
            normalized["evidence"] = {
                k: v
                for k, v in decision.get("evidence", {}).items()
                if k
                in (
                    "semantic_relationship",
                    "byte_identity",
                    "source_currentness",
                    "lineage_requirement",
                    "accepted_source_revision",
                    "output_digest",
                    "layout_digest",
                    "component_package",
                    "component_package_signature",
                    "occurrence_id",
                )
            }
    return {"kind": kind, "value": value}


def compare(previous, current):
    if not current["complete"]:
        raise CatabolicError("incomplete_evaluation_has_no_baseline")
    value = semantic(current)
    return {"changed": previous != value, "baseline": value}
