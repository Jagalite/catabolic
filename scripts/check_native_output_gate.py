#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Prove optimized builds cannot bypass the unresolved destructive-leaf gate."""

import hashlib
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from uuid import uuid4

from catabolic.app import Application
from catabolic.reconcile import Reconciler
from catabolic.store import Store


def check(binary):
    checked = []
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
                app.bind("output", "global", str(output))
                app.scan("media")
                files = {
                    row["path"]: row["id"] for row in store.rows("SELECT * FROM files")
                }
                item = app.put_item("movie", {"gate": kind}, {"title": "Gate"})["id"]
                app.put_mapping("global", files["one.txt"], item, "one.txt")
                assert Reconciler(app).apply()["healthy"]
                link = output / "one.txt"
                original = link.lstat()
                previous = os.readlink(link)
                with store.transaction() as db:
                    if kind == "replace":
                        db.execute("UPDATE mappings SET file_id=?", (files["two.txt"],))
                    else:
                        db.execute("UPDATE mappings SET active=0")
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
                    assert any(
                        "concurrent external leaf-writer safety" in row["reason"]
                        for row in result["blockers"]
                    )
                    assert link.lstat() == original
                    checked.append([kind, action, opt_in])
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
                            kind,
                            os.path.relpath(source / "two.txt", output)
                            if kind == "replace"
                            else None,
                            previous,
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
            assert (
                run.returncode == 2
                and "concurrent external leaf-writer safety" in run.stderr
            ), run
            assert link.lstat() == original
            with Store(database) as store:
                assert len(store.rows("SELECT * FROM journal")) == 1
            assert (source / "one.txt").read_text() == "original source bytes"
            assert (source / "two.txt").read_text() == "replacement source bytes"
            checked.append([kind, "recover", True])
    return {
        "binary": str(binary.resolve()),
        "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "checked": checked,
        "safe_gate": "closed",
        "cases": len(checked),
    }


if __name__ == "__main__":
    print(json.dumps(check(Path(sys.argv[1])), indent=2))
