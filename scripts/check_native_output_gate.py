#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Prove optimized builds cannot bypass the unresolved destructive-leaf gate."""

import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path
from uuid import uuid4

from catabolic.app import Application
from catabolic.reconcile import Reconciler
from catabolic.store import Store

if not __debug__:
    raise RuntimeError("output gate qualification requires unoptimized Python")


def check_fallback(binary):
    """Every generic fallback effect entry refuses even explicit release opt-in."""
    import copy

    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    from catabolic.fallback_projection import FallbackProjection
    from catabolic.layouts import PRESETS, Layouts
    from catabolic.saved_queries import Queries
    from tests.test_fallback_resolution import ResolverTest

    fixture = ResolverTest()
    fixture.setUp()
    try:
        policy = fixture.policy()
        query = Queries(fixture.store).put(
            "gate-members",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT id AS item_id FROM items",
                }
            },
        )["id"]
        Layouts(fixture.app).put("gate-flat", copy.deepcopy(PRESETS["flat"]))
        FallbackProjection(fixture.app).bind("global", policy, query, "gate-flat")
        fixture.store.close()
        before = hashlib.sha256(fixture.database.read_bytes()).hexdigest()
        checked = []
        for surface, action, catalog in (
            ("fallback", "run", "global"),
            ("outputs", "apply", "global"),
            ("outputs", "recover", "global"),
            ("outputs", "apply", None),
            ("outputs", "recover", None),
        ):
            for opt_in in (False, True):
                process = subprocess.run(
                    [
                        str(binary),
                        "--db",
                        str(fixture.database),
                        "operation",
                        surface,
                        action,
                        "--options",
                        json.dumps(
                            {
                                "catalog": catalog,
                                "apply": True,
                                "automatic": True,
                                "disposable_fixture": opt_in,
                            }
                        ),
                    ],
                    capture_output=True,
                    text=True,
                    timeout=30,
                )
                assert process.returncode == 2, process
                expected_gate = (
                    "restricted to disposable debug fixtures"
                    if surface == "fallback"
                    else "only disposable debug fixtures may apply or recover"
                )
                assert expected_gate in process.stderr, process.stderr
                assert (
                    hashlib.sha256(fixture.database.read_bytes()).hexdigest() == before
                )
                assert not list(fixture.output.iterdir())
                checked.append([surface, action, catalog, opt_in])
        return checked
    finally:
        fixture.doCleanups()


def check_links(binary, mode):
    checked = []
    gate = (
        "concurrent external leaf-writer safety"
        if mode == "symlink"
        else "concurrent foreign-leaf relocation safety"
    )
    for kind in ("replace", "remove"):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source, output = root / "source", root / "output"
            source.mkdir()
            output.mkdir()
            (source / "one.txt").write_text("original source bytes")
            (source / "two.txt").write_text("replacement source bytes")
            database = root / "catalog.sqlite3"
            Store.initialize(database)
            with Store(database, writable=True) as store:
                app = Application(store)
                app.bind("source", "media", str(source))
                app.bind("output", "global", str(output), link_mode=mode)
                app.scan("media")
                files = {
                    row["path"]: row["id"] for row in store.rows("SELECT * FROM files")
                }
                item = app.put_item("movie", {"gate": kind}, {"title": "Gate"})["id"]
                app.put_mapping("global", files["one.txt"], item, "one.txt")
                assert Reconciler(app).apply()["healthy"]
                link = output / "one.txt"
                original = link.lstat()
                with store.transaction() as db:
                    if kind == "replace":
                        db.execute("UPDATE mappings SET file_id=?", (files["two.txt"],))
                    else:
                        db.execute("UPDATE mappings SET active=0")
                planned = next(
                    row
                    for row in Reconciler(app).preview()["actions"]
                    if row["kind"] == ("hard_" + kind if mode == "hardlink" else kind)
                )
            for opt_in in (False, True):
                for action in ("preview", "apply"):
                    run = subprocess.run(
                        [
                            str(binary),
                            "--db",
                            str(database),
                            "outputs",
                            action,
                            "--options",
                            json.dumps(
                                {"disposable_fixture": opt_in, "max_removals": 1}
                            ),
                        ],
                        text=True,
                        capture_output=True,
                        timeout=60,
                        check=True,
                    )
                    result = json.loads(run.stdout)
                    assert result["safe"] is False, (kind, opt_in, action, result)
                    assert any(gate in row["reason"] for row in result["blockers"])
                    assert link.lstat() == original
                    checked.append([mode, kind, action, opt_in])
            with Store(database, writable=True) as store:
                assert store.rows("SELECT * FROM journal") == []
                with store.transaction() as db:
                    db.execute(
                        "INSERT INTO journal VALUES(?,?,?,?,?,?,?)",
                        (
                            str(uuid4()),
                            "default",
                            "global",
                            "one.txt",
                            planned["kind"],
                            planned["target"],
                            planned["previous"],
                        ),
                    )
            run = subprocess.run(
                [
                    str(binary),
                    "--db",
                    str(database),
                    "outputs",
                    "recover",
                    "--options",
                    json.dumps({"disposable_fixture": True}),
                ],
                text=True,
                capture_output=True,
                timeout=60,
            )
            assert run.returncode == 2 and gate in run.stderr, run
            assert link.lstat() == original
            with Store(database) as store:
                assert len(store.rows("SELECT * FROM journal")) == 1
            assert (source / "one.txt").read_text() == "original source bytes"
            assert (source / "two.txt").read_text() == "replacement source bytes"
            checked.append([mode, kind, "recover", True])
    return checked


def check(binary):
    checked = [
        *check_links(binary, "symlink"),
        *check_links(binary, "hardlink"),
        *check_fallback(binary),
    ]
    return {
        "binary": str(binary.resolve()),
        "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "checked": checked,
        "safe_gate": "closed",
        "cases": len(checked),
    }


if __name__ == "__main__":
    print(json.dumps(check(Path(sys.argv[1])), indent=2))
