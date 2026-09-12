# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""One ordered resolver shared by projections, HTTP and processing admission."""

import hashlib
import json
import time
from uuid import UUID, uuid5

from .component_packages import PackageBlocked
from .content_access import authorize, revision_of
from .copy_selection import rank_candidate
from .curation import bounded_rows, occurrence
from .domain import CatabolicError
from .fallback_models import Decision, LogicalEntry
from .fallback_policies import Policies
from .fallback_probe import probe
from .outputs import RENDITIONS_SQL
from .rendition_publication import fallback_evidence
from .saved_queries import Queries
from .store import encode


def epoch(store):
    return store.rows("SELECT generation FROM fallback_epoch WHERE id=1")[0][
        "generation"
    ]


def entry_id(store, profile, catalog, entry):
    return str(
        uuid5(UUID(store.database_id), encode(["fallback", profile, catalog, entry]))
    )


def require_epoch(store, expected):
    if epoch(store) != expected:
        raise CatabolicError("stale_resolution_plan")


class Resolver:
    def __init__(self, app, access=None, operation_id=None, session=None):
        self.app, self.store, self.profile = app, app.store, app.profile
        self.access = access
        self.operation_id = operation_id
        self.session = session
        self.primary_choices = {}
        self.total_checks = 0

    def resolve(
        self, entries, policy_id, *, catalog="", previous=None, manual_failback=False
    ):
        if self.store.db.in_transaction or self.store.lock_fd is None:
            raise CatabolicError(
                "resolution requires a short writable session outside a transaction"
            )
        policy = Policies(self.store, self.profile).get(policy_id)["definition"]
        limits = policy["budgets"]
        check_limit = max(0, limits["max_checks"] - self.total_checks)
        if not entries or len(entries) > limits["max_entries"]:
            raise CatabolicError("resolution_entry_budget")
        entries = [LogicalEntry.model_validate(e).model_dump() for e in entries]
        captured_epoch = epoch(self.store)
        previous = previous or {}
        decisions = {}
        captures = {}
        tiers = [
            {"name": t["name"], "query_id": t["query_id"], "state": "not_evaluated"}
            for t in policy["fallbacks"]
        ]
        from .evaluation import EvaluationSession

        if self.session is None:
            self.session = EvaluationSession(
                self.store,
                self.profile,
                http=self.access is not None,
                access=self.access if policy.get("package") else None,
                timeout_ms=limits["query_timeout_ms"],
                max_ids=limits["max_candidates"],
            )
        self.session.check(self.store, self.profile)
        context = self.session.context
        checks = {}
        suppressed = set()
        checked = 0
        candidate_count = 0
        health = []

        def check(snapshots, location):
            nonlocal checked
            key = encode(snapshots)
            if key not in checks:
                if checked >= check_limit:
                    raise PackageBlocked("resolution_probe_budget")
                if location in suppressed:
                    return False
                checked += 1
                before = time.monotonic()
                with self.store.detached():
                    result = probe(
                        self.store.path, snapshots, limits["probe_timeout_ms"]
                    )
                context["deadline"] += time.monotonic() - before
                require_epoch(self.store, captured_epoch)
                checks[key] = result["usable"]
                if result["reason"] in ("probe_timeout", "probe_capacity"):
                    suppressed.add(location)
            return checks[key]

        for entry in entries:
            key = entry_id(self.store, self.profile, catalog, entry)
            decisions[key] = Decision(
                entry_id=key, policy_id=policy_id, state="unresolved", **entry
            ).model_dump()
        queries = Queries(self.store, self.profile)
        for index, tier in enumerate(policy["fallbacks"]):
            pending = [d for d in decisions.values() if d["state"] == "unresolved"]
            if not pending:
                break
            try:
                entity, identifiers, report = queries.select(
                    tier["query_id"], session=self.session
                )
                if not report["complete"]:
                    raise CatabolicError("incomplete_candidate_query")
                tiers[index].update(state="complete", selected_ids=len(identifiers))
                component_selection = None
                if entity in ("component_id", "occurrence_id"):
                    if not policy.get("package"):
                        raise CatabolicError(
                            "component candidates require a package policy"
                        )
                    from .component_sql import OCCURRENCES_SQL

                    component_rows = self.session.rows(
                        "SELECT occurrence_id,association_id FROM ("
                        + OCCURRENCES_SQL
                        + ") WHERE profile=? AND kind='video' AND current=1 AND "
                        + entity
                        + " IN (SELECT value FROM json_each(?)) LIMIT ?",
                        (
                            self.profile,
                            encode(sorted(identifiers)),
                            limits["max_candidates"] + 1,
                        ),
                    )
                    if len(component_rows) > limits["max_candidates"]:
                        raise CatabolicError("resolution_candidate_budget")
                    component_selection = {}
                    for c in component_rows:
                        component_selection.setdefault(c["association_id"], []).append(
                            c["occurrence_id"]
                        )
                    entity, identifiers = "association_id", set(component_selection)
                rows, truncated = bounded_rows(
                    self.store,
                    "SELECT a.*,f.path,f.location FROM item_files a JOIN files f ON f.id=a.file_id WHERE (a.active=1 OR EXISTS (SELECT 1 FROM media_outputs mo JOIN output_definitions od ON od.id=mo.definition_id WHERE mo.file_id=a.file_id AND mo.item_id=a.item_id AND mo.profile=? AND a.role=json_extract(od.definition,'$.role'))) AND a.item_id IN (SELECT value FROM json_each(?)) AND "
                    + {
                        "file_id": "a.file_id",
                        "association_id": "a.id",
                        "item_id": "a.item_id",
                    }[entity]
                    + " IN (SELECT value FROM json_each(?)) ORDER BY a.id LIMIT ?",
                    (
                        self.profile,
                        encode(sorted({d["item_id"] for d in pending})),
                        encode(sorted(identifiers)),
                        limits["max_candidates"] + 1,
                    ),
                    limits["max_candidates"],
                )
                if truncated:
                    raise CatabolicError("resolution_candidate_budget")
                if component_selection is not None:
                    expanded = []
                    for r in rows:
                        for identifier in component_selection[r["id"]]:
                            expanded.append({**r, "video_occurrence_id": identifier})
                    rows = expanded
                candidate_count += len(rows)
                if candidate_count > limits["max_candidates"]:
                    raise CatabolicError("resolution_candidate_budget")
            except CatabolicError as exc:
                tiers[index].update(
                    state="blocked",
                    reason=str(exc) if not self.access else "candidate_query_failed",
                )
                for decision in pending:
                    decision.update(state="blocked", reasons=["candidate_query_failed"])
                break
            # Query budget measures database evaluation, not time spent on bounded probes.
            indexed = {}
            for row in rows:
                metadata = json.loads(row["metadata"])
                signature = (
                    row["item_id"],
                    row["role"],
                    row["part"],
                    metadata.get("variant", ""),
                )
                indexed.setdefault(signature, []).append(row)
            for decision in pending:
                signature = tuple(
                    decision[k] for k in ("item_id", "role", "part", "variant")
                )
                candidates = {}
                for row in indexed.get(signature, []):
                    candidate_key = (row["file_id"], row.get("video_occurrence_id"))
                    if candidate_key in candidates:
                        continue
                    try:
                        captured = self.capture(row, policy, catalog, check=check)
                        if captured is not None:
                            candidates[candidate_key] = captured
                    except PackageBlocked as exc:
                        decision.update(
                            state="blocked",
                            reasons=[
                                str(exc)
                                if not self.access
                                else "component_package_blocked"
                            ],
                        )
                        break
                    except CatabolicError:
                        decision["reasons"].append("candidate_evidence_unavailable")
                if decision["state"] == "blocked":
                    continue
                ranked = sorted(
                    candidates.values(), key=lambda c: (c["rank"], c["file_id"])
                )
                usable = []
                for candidate in ranked:
                    if usable and candidate["rank"] != usable[0]["rank"]:
                        break
                    try:
                        live = check(candidate["checks"], candidate["location"])
                    except PackageBlocked as exc:
                        decision.update(state="blocked", reasons=[str(exc)])
                        break
                    if live:
                        usable.append(candidate)
                        if policy["within_tier"].get(
                            "tie_break"
                        ) == "file_id" and not policy.get("package"):
                            break
                if decision["state"] == "blocked":
                    continue
                if (
                    policy.get("package")
                    and policy["within_tier"].get("tie_break") == "file_id"
                    and usable
                ):
                    first_file = min(c["file_id"] for c in usable)
                    usable = [c for c in usable if c["file_id"] == first_file]
                if len(usable) > 1:
                    decision.update(state="blocked", reasons=["ambiguous_tier"])
                    continue
                if not usable:
                    continue
                chosen = usable[0]
                prior = previous.get(decision["entry_id"])
                if (
                    prior
                    and prior.get("file_id") != chosen["file_id"]
                    and prior.get("tier") is not None
                    and index < prior["tier"]
                ):
                    # A preferred candidate may be delayed only while the old
                    # candidate remains eligible in its pinned tier and usable.
                    old_tier = (
                        policy["fallbacks"][prior["tier"]]
                        if prior["tier"] < len(policy["fallbacks"])
                        else None
                    )
                    if (
                        old_tier
                        and policy["failback"]["mode"] != "immediate"
                        and not manual_failback
                    ):
                        old_entity, old_ids, old_report = queries.select(
                            old_tier["query_id"], session=self.session
                        )
                        prior_evidence = prior.get("evidence", {})
                        if isinstance(prior_evidence, str):
                            prior_evidence = json.loads(prior_evidence)
                        old_package = prior_evidence.get("component_package", {})
                        old_file = old_package.get("video", {}).get(
                            "file_id", prior["file_id"]
                        )
                        old_video = old_package.get("video", {}).get("occurrence_id")
                        if old_entity in ("component_id", "occurrence_id"):
                            from .component_sql import OCCURRENCES_SQL

                            matches = self.store.rows(
                                "SELECT association_id FROM ("
                                + OCCURRENCES_SQL
                                + ") WHERE profile=? AND occurrence_id=? AND "
                                + old_entity
                                + " IN (SELECT value FROM json_each(?)) AND current=1",
                                (self.profile, old_video, encode(sorted(old_ids))),
                            )
                            old_entity, old_ids = (
                                "association_id",
                                {r["association_id"] for r in matches},
                            )
                        old_rows = self.store.rows(
                            "SELECT a.*,f.path,f.location FROM item_files a JOIN files f ON f.id=a.file_id WHERE a.file_id=? AND a.item_id=? AND a.role=? AND a.part IS ?",
                            (
                                old_file,
                                decision["item_id"],
                                decision["role"],
                                decision["part"],
                            ),
                        )
                        old = next(
                            (
                                r
                                for r in old_rows
                                if r[
                                    {
                                        "file_id": "file_id",
                                        "association_id": "id",
                                        "item_id": "item_id",
                                    }[old_entity]
                                ]
                                in old_ids
                                and json.loads(r["metadata"]).get("variant", "")
                                == decision["variant"]
                            ),
                            None,
                        )
                        if not old_report["complete"]:
                            raise CatabolicError("incomplete_candidate_query")
                        tiers[prior["tier"]].update(
                            state="complete", selected_ids=len(old_ids)
                        )
                        try:
                            old_capture = (
                                self.capture(
                                    {
                                        **old,
                                        **(
                                            {"video_occurrence_id": old_video}
                                            if old_video
                                            else {}
                                        ),
                                    },
                                    policy,
                                    catalog,
                                    check=check,
                                )
                                if old
                                else None
                            )
                        except PackageBlocked as exc:
                            decision.update(state="blocked", reasons=[str(exc)])
                            continue
                        except CatabolicError:
                            old_capture = None
                        if old_capture:
                            try:
                                old_live = check(
                                    old_capture["checks"], old_capture["location"]
                                )
                            except PackageBlocked as exc:
                                decision.update(state="blocked", reasons=[str(exc)])
                                continue
                            if old_live:
                                observation = self.health(
                                    decision["entry_id"], chosen, catalog
                                )
                                health.append(observation)
                                stable = policy["failback"]
                                if (
                                    stable["mode"] == "manual"
                                    or observation["successes"]
                                    < stable["minimum_successful_checks"]
                                    or time.time() - observation["healthy_since"]
                                    < stable["minimum_healthy_seconds"]
                                ):
                                    chosen = old_capture
                                    selected_index = prior["tier"]
                                else:
                                    selected_index = index
                            else:
                                selected_index = index
                        else:
                            selected_index = index
                    else:
                        selected_index = index
                else:
                    selected_index = index
                if prior and prior.get("file_id") != chosen["file_id"]:
                    previous_identity = json.loads(prior.get("evidence", "{}")).get(
                        "byte_identity"
                    )
                    identity = chosen["evidence"].get("byte_identity")
                    if previous_identity and identity:
                        chosen["evidence"]["byte_relationship"] = (
                            "recorded_matching_digest_and_length"
                            if identity == previous_identity
                            else "known_different_content"
                        )
                        chosen["evidence"]["compared_revision"] = prior["revision"]
                decision.update(
                    state="resolved_preferred"
                    if selected_index == 0
                    else "resolved_fallback",
                    selected_tier=selected_index,
                    selected_tier_name=policy["fallbacks"][selected_index]["name"],
                    file_id=chosen["file_id"],
                    revision=chosen["revision"],
                    rendition_id=chosen["rendition_id"],
                    content_path=f"/v1/files/{chosen['file_id']}/content?revision={chosen['revision']}",
                    evidence=chosen["evidence"],
                )
                package = chosen["evidence"].get("component_package")
                if package and not package["packaging"]["ready"]:
                    decision["content_path"] = None
                captures[decision["entry_id"]] = chosen
        for decision in decisions.values():
            if decision["state"] == "unresolved":
                decision["reasons"].append("no_usable_candidate")
        # Multipart slots require an explicit coherent representation group.
        for item in {e["item_id"] for e in entries if e["part"] is not None}:
            group = [d for d in decisions.values() if d["item_id"] == item]
            groups = {
                captures[d["entry_id"]]["metadata"].get("representation_group")
                for d in group
                if d["entry_id"] in captures
            }
            if (
                len(groups) != 1
                or None in groups
                or any(not d["state"].startswith("resolved") for d in group)
            ):
                for d in group:
                    d.update(
                        state="blocked",
                        file_id=None,
                        revision=None,
                        content_path=None,
                        reasons=["multipart_coverage_ambiguous"],
                    )
        if policy.get("package"):
            for decision in decisions.values():
                if not decision["state"].startswith("resolved"):
                    package = decision["evidence"].setdefault(
                        "component_package",
                        {"version": 1, "components": [], "dependencies": []},
                    )
                    package["packaging"] = {
                        "action": "block",
                        "ready": False,
                        "reason": decision["reasons"][-1]
                        if decision["reasons"]
                        else "no_coherent_component_package",
                        "exposes_unselected_components": False,
                    }
        require_epoch(self.store, captured_epoch)
        self.total_checks += checked
        result = {
            "policy_id": policy_id,
            "epoch": captured_epoch,
            "decisions": list(decisions.values()),
            "tiers": tiers,
            "checks": checked,
            "health": health,
            "captures": captures,
        }
        result["plan_id"] = hashlib.sha256(
            encode({k: v for k, v in result.items() if k != "health"}).encode()
        ).hexdigest()
        return result

    def health(self, entry, candidate, catalog):
        now = time.time()
        rows = self.store.rows(
            "SELECT * FROM fallback_health WHERE profile=? AND catalog=? AND entry_id=? AND file_id=? AND revision=?",
            (self.profile, catalog, entry, candidate["file_id"], candidate["revision"]),
        )
        old = rows[0] if rows else None
        return {
            "profile": self.profile,
            "catalog": catalog,
            "entry_id": entry,
            "file_id": candidate["file_id"],
            "revision": candidate["revision"],
            "healthy_since": old["healthy_since"] if old else now,
            "successes": old["successes"] + 1 if old else 1,
            "last_check": now,
        }

    def capture(self, row, policy, catalog, check=None):
        revision = revision_of(self.store, self.profile, row["file_id"])
        if self.access:
            if self.operation_id:
                if not self.access.processing(
                    {
                        "item_id": row["item_id"],
                        "source_file_id": row["file_id"],
                        "source_revision": revision,
                        "operation_id": self.operation_id,
                    }
                ):
                    return None
            else:
                authorize(self.access, row["file_id"], revision)
        ranking = rank_candidate(
            self.store, self.profile, row["file_id"], policy["within_tier"]
        )
        if ranking["required_fields_failed"]:
            return None
        metadata = json.loads(row["metadata"])
        if metadata.get("for_file_id") and metadata[
            "for_file_id"
        ] != self.primary_choices.get(row["item_id"]):
            return None
        outputs = self.store.rows(
            f"SELECT * FROM ({RENDITIONS_SQL}) WHERE profile=? AND file_id=?",
            (self.profile, row["file_id"]),
        )
        if not row["active"] and not outputs:
            return None
        evidence = {
            "semantic_relationship": "curated_association",
            "byte_relationship": "not_established",
            "source_currentness": "not_applicable",
        }
        checks = [occurrence(self.store, self.profile, row["file_id"])]
        rendition_id = None
        if outputs:
            if len(outputs) != 1:
                raise CatabolicError("ambiguous_output_lineage")
            output = outputs[0]
            if output["item_id"] != row["item_id"] or (
                row["role"] == "primary"
                and output["purpose"] not in ("transcode", "remux")
            ):
                return None
            if self.store.rows(
                "SELECT 1 FROM rendition_decisions WHERE profile=? AND catalog=? AND output_id=? AND excluded=1",
                (self.profile, catalog, output["id"]),
            ):
                return None
            checks, lineage = fallback_evidence(
                self.app, output, policy["lineage_requirement"]
            )
            if self.access:
                lineage.pop("accepted_source_revision", None)
            evidence.update(semantic_relationship="accepted_derivative", **lineage)
            rendition_id = output["id"]
            evidence["byte_identity"] = {
                "algorithm": "sha256",
                "digest": lineage["output_digest"],
                "size": str(checks[0]["size"]),
            }
        else:
            from .processing import current_fact

            for operation in ("verify", "hash"):
                fact = current_fact(self.store, self.profile, row["file_id"], operation)
                if (
                    fact
                    and fact["current"]
                    and fact["data"].get("digest")
                    and "ctime_ns" in fact["snapshot"]
                ):
                    checks[0]["ctime_ns"] = fact["snapshot"]["ctime_ns"]
                    evidence["byte_identity"] = {
                        "algorithm": "sha256",
                        "digest": fact["data"]["digest"],
                        "size": str(checks[0]["size"]),
                    }
                    break
        captured = {
            **row,
            "metadata": metadata,
            "revision": revision,
            "rank": ranking["rank"],
            "checks": checks,
            "evidence": evidence,
            "rendition_id": rendition_id,
        }

        if policy.get("package"):
            from .component_packages import capture

            try:
                return capture(self, captured, policy, check=check)
            except CatabolicError as exc:
                raise PackageBlocked(str(exc)) from exc
        return captured
