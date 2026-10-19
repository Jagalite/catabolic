# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Cross-feature native parser/validation source views, using frozen Python only
as an independent oracle. No wrapper, service or filesystem mutation is tested.
"""

import hashlib
import json
import os
import subprocess
import unittest
from pathlib import Path

from tests import test_rust_legacy_inbox as fixture


def cases():
    yield (
        "review-original",
        "query($f:RenditionFilter={current:true,current:false} "
        "@other(arg:{a:1,a:2})){renditions(matching:$f){nodes}}",
    )
    for newline_name, newline in (("lf", "\n"), ("cr", "\r"), ("crlf", "\r\n")):
        for text_name, text in (
            ("ascii", "plain"),
            ("escaped-pair", r"\uD83D\uDE00"),
            ("literal-unicode", "😀雪"),
        ):
            for moved in (False, True):
                lines = [
                    "query(",
                    "$f:RenditionFilter={",
                    f'purpose:"{text}",',
                    "current:true,",
                    "current:false",
                    "}",
                ]
                if moved:
                    lines += [
                        "@other(arg:{",
                        f'label:"{text}",',
                        "a:1,a:2",
                        "})",
                    ]
                lines += [") {", "renditions(matching:$f){nodes}", "}"]
                yield (
                    f"{newline_name}-{text_name}-directive-{moved}",
                    newline.join(lines),
                )


class ComposedViewsTest(unittest.TestCase):
    setUp = fixture.NativeLegacyInboxTest.setUp
    invoke = fixture.NativeLegacyInboxTest.invoke

    def test_duplicate_validation_and_parser_views_keep_original_positions(self):
        companion = Path(os.environ["CATABOLIC_NATIVE_BINARY"])
        self.assertTrue(companion.is_file())
        paths = (self.reference, self.native)
        before = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}
        results = []
        for name, document in cases():
            arguments = ["graphql", document, "--variables", "{}"]
            reference = self.invoke(True, ["--machine", *arguments])
            native = self.invoke(False, ["--machine", *arguments])
            structured = subprocess.run(
                [str(companion), "--db", str(self.native), "--json", *arguments],
                env=self.env,
                capture_output=True,
                text=True,
                timeout=90,
            )

            def complete(process):
                return dict(
                    code=process.returncode,
                    stderr=process.stderr,
                    stdout=json.loads(process.stdout),
                )

            expected, actual, actual_companion = map(
                complete, (reference, native, structured)
            )
            expected_companion = dict(
                code=0, stderr="", stdout=expected["stdout"]["data"]
            )
            match = (
                expected["code"] == 2
                and fixture.parser_fixture.strict(actual)
                == fixture.parser_fixture.strict(expected)
                and fixture.parser_fixture.strict(actual_companion)
                == fixture.parser_fixture.strict(expected_companion)
            )
            results.append(
                dict(
                    name=name,
                    document=document,
                    match=match,
                    reference=expected,
                    legacy=actual,
                    companion=actual_companion,
                )
            )
        if report := os.environ.get("CATABOLIC_GRAPHQL_COMPOSED_VIEWS_REPORT"):
            Path(report).write_text(json.dumps(results, indent=2) + "\n")
        self.assertEqual(len(results), 19)
        self.assertFalse(
            [result for result in results if not result["match"]],
            json.dumps(results, indent=2),
        )
        self.assertEqual(
            {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}, before
        )
        print(f"Composed parser/validation CLI pairs: {len(results)}")


if __name__ == "__main__":
    unittest.main()
