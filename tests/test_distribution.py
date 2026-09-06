# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Release gates reject mismatched tags, incomplete archives and altered resources."""

import io
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

from scripts.check_distribution import check_distribution, check_version


class DistributionTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.dist = self.root / "dist"
        self.dist.mkdir()
        contents = {
            "pyproject.toml": '[project]\nname="catabolic"\nversion="0.1.0"\nrequires-python=">=3.11"\nlicense="MIT"\n',
            "src/catabolic/__init__.py": '__version__ = "0.1.0"\n',
            "src/catabolic/migrations/001_initial.sql": "SELECT 1;\n",
            "src/catabolic/migrations/001_initial.sql.license": "fixture notice\n",
            "src/catabolic/guides/GUIDE.md": "Fixture guide\n",
            "docs/GUIDE.md": "Fixture guide\n",
            "README.md": "Fixture README\n",
            "LICENSE": "Fixture license\n",
        }
        for name, body in contents.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(body)
        self.contents = {name: body.encode() for name, body in contents.items()}
        self.metadata = (
            "Metadata-Version: 2.4\nName: catabolic\nVersion: 0.1.0\n"
            "Requires-Python: >=3.11\nLicense-Expression: MIT\nLicense-File: LICENSE\n\n"
        ).encode()

    def artifacts(self, omit=None, tamper=None):
        wheel_name = self.dist / "catabolic-0.1.0-py3-none-any.whl"
        with zipfile.ZipFile(wheel_name, "w") as wheel:
            wheel.writestr("catabolic-0.1.0.dist-info/METADATA", self.metadata)
            wheel.writestr(
                "catabolic-0.1.0.dist-info/licenses/LICENSE", b"Fixture license\n"
            )
            for name, body in self.contents.items():
                if name.startswith("src/") and name != omit:
                    wheel.writestr(
                        name.removeprefix("src/"),
                        b"changed" if name == tamper else body,
                    )
        with tarfile.open(self.dist / "catabolic-0.1.0.tar.gz", "w:gz") as archive:
            for name, body in {**self.contents, "PKG-INFO": self.metadata}.items():
                entry = tarfile.TarInfo("catabolic-0.1.0/" + name)
                entry.size = len(body)
                archive.addfile(entry, io.BytesIO(body))

    def test_matching_package_and_tag(self):
        self.artifacts()
        result = check_distribution(self.root, self.dist, "refs/tags/v0.1.0", True)
        self.assertEqual(len(result["files"]), 2)
        self.assertTrue(all(len(f["sha256"]) == 64 for f in result["files"]))

    def test_publish_requires_exact_tag(self):
        for ref in ("", "refs/heads/main", "refs/tags/v0.2.0", "refs/tags/0.1.0"):
            with self.subTest(ref=ref), self.assertRaises(ValueError):
                check_version(self.root, ref, require_tag=True)
        self.assertEqual(
            check_version(self.root, "refs/heads/main")["version"], "0.1.0"
        )

    def test_runtime_version_must_match_metadata(self):
        (self.root / "src/catabolic/__init__.py").write_text('__version__ = "0.2.0"\n')
        with self.assertRaises(ValueError):
            check_version(self.root)

    def test_missing_guide_and_modified_migration_are_rejected(self):
        self.artifacts(omit="src/catabolic/guides/GUIDE.md")
        with self.assertRaises(KeyError):
            check_distribution(self.root, self.dist)
        self.artifacts(tamper="src/catabolic/migrations/001_initial.sql")
        with self.assertRaises(ValueError):
            check_distribution(self.root, self.dist)

    def test_stale_distributions_are_rejected(self):
        self.artifacts()
        (self.dist / "catabolic-0.0.1.whl").write_bytes(b"stale")
        with self.assertRaises(ValueError):
            check_distribution(self.root, self.dist)


if __name__ == "__main__":
    unittest.main()
