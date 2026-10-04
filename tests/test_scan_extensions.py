# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import unittest

from catabolic import observations
from catabolic.domain import CatabolicError
from catabolic.scan_policy import MEDIA_EXTENSIONS, configure
from tests import test_guarded_scans as fixtures


class ScanExtensionTest(unittest.TestCase):
    setUp = fixtures.GuardedScanTest.setUp
    files = fixtures.GuardedScanTest.files

    def test_default_inventory_and_case_insensitive_sidecars(self):
        for name in (
            "movie.MKV",
            "external.mka",
            "captions.srt",
            "fonts.ttf",
            "book.epub",
            "script.py",
        ):
            (self.source / name).write_text("fixture")
        result = self.app.scan("media")
        self.assertTrue(result["complete"])
        paths = {r["path"] for r in self.files()}
        self.assertTrue(
            {"movie.MKV", "external.mka", "captions.srt", "fonts.ttf", "book.epub"}
            <= paths
        )
        self.assertNotIn("script.py", paths)
        self.assertEqual(result["scans"][0]["extensions"], MEDIA_EXTENSIONS)

    def test_all_files_then_narrow_never_marks_skipped_missing(self):
        configure(self.app, "media", {"extensions": None}, apply=True)
        for name in ("movie.mkv", "script.py", "README"):
            (self.source / name).write_text("fixture")
        self.assertTrue(self.app.scan("media")["complete"])
        previous = {r["path"]: r for r in self.files()}
        for name in ("script.py", "README", "movie.mkv"):
            (self.source / name).unlink()
        configure(self.app, "media", {"extensions": [".mkv"]}, apply=True)
        self.assertTrue(self.app.scan("media")["complete"])
        current = {r["path"]: r for r in self.files()}
        self.assertEqual(current["movie.mkv"]["status"], "missing")
        for name in ("script.py", "README"):
            self.assertEqual(current[name], previous[name])
        configure(self.app, "media", {"extensions": None}, apply=True)
        self.assertTrue(self.app.scan("media")["complete"])
        self.assertTrue(
            all(
                r["status"] == "missing"
                for r in self.files()
                if r["path"] in ("script.py", "README")
            )
        )

    def test_policy_revision_reuse_and_explicit_extensions(self):
        (self.source / "document.custom").write_text("fixture")
        old = observations.request(self.app, "media")
        preview = configure(self.app, "media", {"extensions": [".CUSTOM", ".custom"]})
        self.assertEqual(preview["policy"]["extensions"], [".custom"])
        configure(self.app, "media", {"extensions": [".CUSTOM"]}, apply=True)
        new = observations.request(self.app, "media")
        self.assertNotEqual(old["id"], new["id"])
        result = self.app.scan("media", extended=True)
        self.assertTrue(result["complete"])
        self.assertIn("document.custom", {r["path"] for r in self.files()})
        self.assertEqual(result["scans"][0]["extensions"], [".custom"])
        for invalid in ([], "*.mkv", ["*.mkv"], ["../mkv"], [1]):
            with self.assertRaises(CatabolicError):
                configure(self.app, "media", {"extensions": invalid})

    def test_regex_filters_skip_new_files_without_catalog_rows(self):
        configure(
            self.app,
            "media",
            {
                "include_regex": [r"(?i)^films/.*\.mkv$"],
                "exclude_regex": [r"(?i)/sample[^/]*$"],
            },
            apply=True,
        )
        (self.source / "films").mkdir()
        for name in ("feature.MKV", "sample.mkv", "readme.txt"):
            (self.source / "films" / name).write_text("fixture")
        result = self.app.scan("media")
        self.assertTrue(result["complete"])
        self.assertEqual(
            [r["path"] for r in self.files()], ["example.mkv", "films/feature.MKV"]
        )
        self.assertEqual(
            self.store.rows("SELECT count(*) AS n FROM observations")[0]["n"], 2
        )
        for bad in ([r"["], ".*", ["x" * 1025]):
            with self.assertRaises(CatabolicError):
                configure(self.app, "media", {"include_regex": bad})

    def test_regex_narrowing_retains_prior_evidence_and_timeout_stops(self):
        from unittest.mock import Mock, patch

        from catabolic.scan_policy import included
        from catabolic.scan_traversal import ScanResourceStop

        (self.source / "old.mkv").write_text("fixture")
        self.app.scan("media")
        before = next(r for r in self.files() if r["path"] == "old.mkv")
        (self.source / "old.mkv").unlink()
        configure(self.app, "media", {"exclude_regex": [r"^old\.mkv$"]}, apply=True)
        self.assertTrue(self.app.scan("media")["complete"])
        self.assertEqual(
            before, next(r for r in self.files() if r["path"] == "old.mkv")
        )
        with patch(
            "catabolic.scan_policy.compile_pattern",
            return_value=Mock(search=Mock(side_effect=TimeoutError)),
        ):
            with self.assertRaisesRegex(ScanResourceStop, "regex_filter_timeout"):
                included("file.mkv", None, ["pattern"])
