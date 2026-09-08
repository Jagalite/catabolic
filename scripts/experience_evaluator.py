# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Read-only evaluation of retained experience fixtures; no Catabolic imports."""

import argparse
import hashlib
import json
import os
import sqlite3
from pathlib import Path


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def snapshot(root):
    """Semantic DB state and actual links, independently of CLI success claims."""
    root = Path(root).resolve()
    with sqlite3.connect(
        (root / "catalog.sqlite3").as_uri() + "?mode=ro", uri=True
    ) as db:

        def rows(sql):
            return [list(row) for row in db.execute(sql)]

        associations = rows("""SELECT f.location,f.path,i.namespace,i.value,a.role,a.active
            FROM item_files a JOIN files f ON f.id=a.file_id
            LEFT JOIN identities i ON i.item_id=a.item_id ORDER BY 1,2,3,4,5,6""")
        # Retain every accepted proposal, even if its association was later corrected.
        accepted = rows("""SELECT f.location,f.path,p.payload FROM proposals p
            JOIN files f ON f.id=p.file_id WHERE p.state='accepted' ORDER BY 1,2,3""")
        proposals = rows(
            "SELECT file_id,state,payload,evidence,source FROM proposals ORDER BY file_id,payload"
        )
        metadata = rows("SELECT id,kind,metadata FROM items ORDER BY id")
        identified_metadata = rows(
            "SELECT i.namespace,i.value,m.kind,m.metadata FROM identities i "
            "JOIN items m ON m.id=i.item_id ORDER BY 1,2"
        )
        relationships = rows(
            "SELECT source_id,target_id,kind,position,active FROM item_relationships ORDER BY 1,2,3"
        )
        curation = rows(
            "SELECT item_id,status,revision,updated_by FROM item_workflow ORDER BY item_id"
        )
        worklog = rows(
            "SELECT item_id,revision,kind,body,actor,data FROM item_worklog ORDER BY item_id,revision"
        )
        pending = db.execute("SELECT count(*) FROM journal").fetchone()[0]
        refresh = db.execute("SELECT count(*) FROM catalog_refresh_queue").fetchone()[0]
        completed = db.execute(
            "SELECT count(*) FROM processing_artifacts WHERE state='ready'"
        ).fetchone()[0]
    links = {}
    unexpected_files = []
    for catalog in ("originals", "mobile"):
        output = root / catalog
        for directory, dirs, files in os.walk(output):
            for name in dirs + files:
                path = Path(directory) / name
                if path.is_symlink():
                    links[str(path.relative_to(root))] = {
                        "target": str(path.resolve().relative_to(root))
                        if path.resolve().is_relative_to(root)
                        else str(path.resolve()),
                        "text": os.readlink(path),
                        "inode": path.lstat().st_ino,
                        "exists": path.exists(),
                    }
                elif path.is_file() and path.name != ".catabolic-owner.json":
                    unexpected_files.append(str(path.relative_to(root)))
    return dict(
        associations=associations,
        accepted=accepted,
        proposals=proposals,
        metadata=metadata,
        identified_metadata=identified_metadata,
        relationships=relationships,
        curation=curation,
        worklog=worklog,
        pending=pending,
        refresh=refresh,
        completed=completed,
        links=links,
        unexpected_files=sorted(unexpected_files),
    )


def evaluate(key, observed, review, checkpoints):
    """Fail closed on missing cases/evidence. Review is human-adjudicated for agents."""
    cases = key["cases"]
    by_path = {(c["location"], c["path"]): c for c in cases}
    wrong, resolved = set(), set()
    for location, path, namespace, identity, role, active in observed["associations"]:
        case = by_path.get((location, path))
        if not case:
            continue  # generated occurrences are evaluated through publication below
        if (
            namespace != key["namespace"]
            or identity != case["identity"]
            or role != case.get("role", "primary")
        ):
            wrong.add(case["id"])
        elif active and identity is not None:
            resolved.add(case["id"])
    for location, path, payload in observed["accepted"]:
        case = by_path.get((location, path))
        if case:
            item = json.loads(payload).get("item")
            if (
                item
                and item.get("identities", {}).get(key["namespace"]) != case["identity"]
            ):
                wrong.add(case["id"])
    decisions = review.get("cases", {})
    missing = [c["id"] for c in cases if c["id"] not in decisions]
    ambiguous = [c["id"] for c in cases if c["identity"] is None]
    deferred = [
        cid
        for cid in ambiguous
        if decisions.get(cid, {}).get("state") == "deferred"
        and decisions[cid].get("reason", "").strip()
        and cid not in wrong
    ]
    required = {c["id"] for c in cases if c["identity"] is not None}
    metadata = {
        (namespace, identity): (kind, json.loads(value))
        for namespace, identity, kind, value in observed.get("identified_metadata", [])
    }
    for case in cases:
        if case["identity"] is not None and "title" in case:
            actual = metadata.get((key["namespace"], case["identity"]))
            if (
                not actual
                or actual[0] != case.get("kind", "movie")
                or any(
                    actual[1].get(field) != case[field] for field in ("title", "year")
                )
            ):
                wrong.add(case["id"])
    confident_wrong = set()
    for case in cases:
        decision = decisions.get(case["id"], {})
        if (
            decision.get("state") == "accepted"
            and decision.get("identity") != case["identity"]
        ):
            confident_wrong.add(case["id"])
    actual_targets = {}
    for path, link in observed["links"].items():
        actual_targets.setdefault(path.split("/")[0], []).append(link["target"])
    publication = all(
        sorted(actual_targets.get(catalog, [])) == sorted(targets)
        for catalog, targets in key["expected_targets"].items()
    )
    publication = publication and all(
        link["exists"] for link in observed["links"].values()
    )
    publication = publication and not observed.get("unexpected_files", [])
    pairs = [
        ("initial", "repeat1"),
        ("repeat1", "repeat2"),
        ("late", "late_repeat1"),
        ("late_repeat1", "late_repeat2"),
    ]
    stable = all(
        a in checkpoints and b in checkpoints and checkpoints[a] == checkpoints[b]
        for a, b in pairs
    )
    recovery = checkpoints.get("recovery", {})
    recovery_ok = all(
        recovery.get(name, {}).get("interrupted_exit") == 77
        and recovery[name].get("pending_before", 0) > 0
        and recovery[name].get("verified") is True
        for name in ("journal", "completion")
    )
    metrics = {
        "incorrect_committed": sorted(wrong),
        "incorrect_confident": sorted(confident_wrong),
        "resolved": len(resolved & required),
        "resolvable": len(required),
        "unnecessary_deferrals": sorted(required - resolved),
        "appropriate_unresolved": len(deferred),
        "ambiguous": len(ambiguous),
        "missing_reviews": missing,
        "human_interventions": len(review.get("interventions", [])),
        "publication_correct": publication,
        "repeat_stable": stable,
        "recovery_verified": recovery_ok,
        "source_preserved": checkpoints.get("source_preserved") is True,
        "outage_recovered": checkpoints.get("outage_recovered") is True,
        "no_pending_work": observed["pending"] == observed["refresh"] == 0,
    }
    passed = (
        not wrong
        and not confident_wrong
        and not missing
        and required <= resolved
        and len(deferred) == len(ambiguous)
        and publication
        and stable
        and recovery_ok
        and metrics["source_preserved"]
        and metrics["outage_recovered"]
        and metrics["no_pending_work"]
    )
    # An agent's own review JSON cannot certify its judgment. Keep the hard
    # mechanical gates useful while requiring a separate transcript adjudication.
    reference = review.get("run_type") == "reference"
    return {
        "passed": passed and reference,
        "mechanical_passed": passed,
        "metrics": metrics,
        "judgment_status": "not_measured"
        if reference
        else "requires_transcript_adjudication",
    }


def audit(root):
    """Re-read live retained artifacts; never accept a saved report as a verdict."""
    root = Path(root).resolve()

    def read(name):
        return json.loads((root / name).read_text())

    key, review, checkpoints = (
        read("answer-key.json"),
        read("review.json"),
        read("checkpoints.json"),
    )
    observed = snapshot(root)
    hashes = read("source-hashes.json")
    actual_paths = {
        str(p.relative_to(root))
        for name in ("a", "b")
        for p in (root / name).rglob("*")
        if p.is_file() and not p.name.startswith(".catabolic-")
    }
    checkpoints["source_preserved"] = actual_paths == set(hashes) and all(
        (root / path).is_file() and digest(root / path) == value
        for path, value in hashes.items()
    )
    result = evaluate(key, observed, review, checkpoints)
    transcript = [
        json.loads(line)
        for line in (root / "transcript.jsonl").read_text().splitlines()
    ]
    interruptions = sum(event["exit"] == 77 for event in transcript)
    evidence_valid = interruptions == 2 and observed == checkpoints.get("late_repeat2")
    result["retained_evidence_valid"] = evidence_valid
    result["passed"] = result["passed"] and evidence_valid
    result["mechanical_passed"] = result["mechanical_passed"] and evidence_valid
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, help="retained reference fixture")
    args = parser.parse_args()
    try:
        result = audit(args.root)
    except (OSError, ValueError, KeyError, sqlite3.Error) as exc:
        result = {"passed": False, "error": str(exc)}
    print(json.dumps(result, indent=2))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
