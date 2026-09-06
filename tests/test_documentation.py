# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from catabolic.documentation import TOPICS, documentation

ROOT = Path(__file__).resolve().parents[1]


class DocumentationTest(unittest.TestCase):
    def test_bundled_guides_match_sources(self):
        result = subprocess.run(
            [sys.executable, str(ROOT / "scripts/sync_docs.py"), "--check"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        identifiers = [entry[0] for entry in TOPICS]
        self.assertEqual(len(set(identifiers)), len(identifiers))
        for topic, filename, _, _ in TOPICS:
            with self.subTest(topic=topic):
                self.assertEqual(
                    documentation(topic)["markdown"],
                    (ROOT / filename).read_text(encoding="utf-8"),
                )

    def test_cli_offline_topics_json_search_and_errors(self):
        with tempfile.TemporaryDirectory() as temporary:
            # Even an explicitly invalid database must never be opened for docs.
            database = Path(temporary) / "missing.sqlite3"
            env = {**os.environ, "CATABOLIC_DB": str(database)}

            def run(*args):
                return subprocess.run(
                    [sys.executable, "-m", "catabolic", *args],
                    cwd=temporary,
                    env=env,
                    capture_output=True,
                    text=True,
                )

            index = run("docs")
            self.assertEqual(index.returncode, 0, index.stderr)
            self.assertIn("catabolic docs TOPIC", index.stdout)
            plain = run("docs", "query")
            self.assertEqual(plain.returncode, 0, plain.stderr)
            self.assertIn("SELECT item_id", plain.stdout)
            self.assertTrue(plain.stdout.startswith("# SQL"))
            for args in (("--json", "docs", "native"), ("docs", "native", "--json")):
                result = run(*args)
                self.assertEqual(result.returncode, 0, result.stderr)
                document = json.loads(result.stdout)
                self.assertEqual(document["documentation_version"], 1)
                self.assertEqual(document["topic"], "native")
                self.assertIn("catabolic.native", document["markdown"])
            result = run("--json", "docs", "--search", "MANIFEST version")
            self.assertEqual(result.returncode, 0, result.stderr)
            topics = json.loads(result.stdout)["topics"]
            self.assertIn("native", {topic["topic"] for topic in topics})
            for entry in topics:
                self.assertLessEqual(len(entry["matches"]), 3)
                lines = documentation(entry["topic"])["markdown"].splitlines()
                for match in entry["matches"]:
                    self.assertEqual(match["text"], lines[match["line"] - 1][:240])
            result = run("docs", "--json", "--search", "no_such_topic_290358")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)["topics"], [])
            for args in (
                ["../../README.md"],
                ["--search", "   "],
                ["query", "--search", "SQL"],
            ):
                result = run("--json", "docs", *args)
                self.assertEqual(result.returncode, 2, result.stdout)
                self.assertEqual(result.stdout, "")
                self.assertIn("message", json.loads(result.stderr)["error"])
            self.assertFalse(database.exists())
            self.assertEqual(list(Path(temporary).iterdir()), [])
