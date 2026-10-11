#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Independent installed-only review of metadata exception and readback fences.

Run outside the checkout with the reviewed wheel's Python and -I. Fixtures use
only temporary catalogs, fake credentials, and numeric loopback. This is
a scoped interface check; final milestone/platform qualification is separate.
"""

import argparse
import hashlib
import importlib.util
import json
import os
import signal
import sys
import threading
import unittest
from pathlib import Path
from urllib.parse import urlsplit

FIXTURE_PATH = Path(__file__).with_name("native_plex_metadata_embedding_tests.py")
spec = importlib.util.spec_from_file_location("_review_metadata_fixture", FIXTURE_PATH)
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class InstalledMetadataReview(fixture.InstalledPlexMetadata):
    def test_review_readback_interrupt_preserves_exact_base_exception(self):
        class ReviewSignal(BaseException):
            pass

        expected = ReviewSignal("independent exact readback interruption")
        handled = threading.Event()

        def signal_handler(_signal, _frame):
            handled.set()
            raise expected

        previous = signal.signal(signal.SIGINT, signal_handler)
        previous_term = signal.getsignal(signal.SIGTERM)
        self.addCleanup(signal.signal, signal.SIGINT, previous)
        preview = self.metadata()
        self.requests.clear()
        original_get = self.server.RequestHandlerClass.do_GET
        owner = self

        def gated_get(handler):
            if urlsplit(handler.path).path == "/library/metadata/1" and any(
                method == "PUT" for method, _ in owner.requests
            ):
                owner.block = ("GET", "/library/metadata/1")
            original_get(handler)

        self.server.RequestHandlerClass.do_GET = gated_get

        def interrupt_after_readback_begins():
            if not self.entered.wait(10):
                self.errors.append("readback did not reach the deterministic gate")
                self.release.set()
                return
            os.kill(os.getpid(), signal.SIGINT)
            if not handled.wait(10):
                self.errors.append("Python did not handle the signal")
            self.release.set()

        sender = threading.Thread(target=interrupt_after_readback_begins)
        sender.start()
        try:
            with self.assertRaises(ReviewSignal) as raised:
                self.metadata(apply=True, expected_plan=preview["plan_id"])
        finally:
            self.release.set()
            sender.join(12)
        self.assertFalse(sender.is_alive())
        self.assertTrue(handled.is_set())
        self.assertIs(raised.exception, expected)
        self.assertIs(signal.getsignal(signal.SIGINT), signal_handler)
        self.assertIs(signal.getsignal(signal.SIGTERM), previous_term)
        self.assertEqual(self.requests[-1], ("GET", "/library/metadata/1"))
        self.assertEqual(self.events()[0]["state"], "started")
        self.assertEqual(self.node.get("title"), "Curated title")
        self.assertEqual(sum(method == "PUT" for method, _ in self.requests), 1)
        self.block = None
        self.server.RequestHandlerClass.do_GET = original_get
        preview = self.metadata()
        resumed = self.metadata(apply=True, expected_plan=preview["plan_id"])
        self.assertEqual(resumed["results"][0]["state"], "unchanged")
        self.assertEqual(sum(method == "PUT" for method, _ in self.requests), 1)
        with self.native.Store(self.path, writable=True):
            pass

    def test_review_malformed_json_errors_are_recoverable_without_panic(self):
        for raw in (
            "[]",
            "null",
            "false",
            "{}",
            '{"broken":',
            '{"item_ids":42,"fields":["title"]}',
        ):
            with self.subTest(raw=raw), self.assertRaises(RuntimeError) as raised:
                self.native._native.execute_operation(
                    str(self.path), "plex-metadata", "run", raw, "default"
                )
            self.assertNotIn("FAKE_ONLY_NO_REAL_ACCOUNT", str(raised.exception))
        self.assertEqual(self.requests, [])
        self.assertEqual(self.events(), [])
        preview = self.metadata()
        self.assertTrue(
            self.metadata(apply=True, expected_plan=preview["plan_id"])["complete"]
        )


def result_verifier():
    path = Path(__file__).resolve().with_name("native_qualification_results.py")
    spec = importlib.util.spec_from_file_location("_required_results", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.run_required_suite


EXPECTED_TESTS = (
    "test_review_readback_interrupt_preserves_exact_base_exception",
    "test_review_malformed_json_errors_are_recoverable_without_panic",
)


def main():
    if not __debug__:
        raise SystemExit("metadata review qualification requires assertions")
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--wheel", type=Path)
    mode.add_argument(
        "--dev", action="store_true", help="Explicit development-only artifact review"
    )
    parser.add_argument("--report", type=Path)
    parser.add_argument("--expected-extension-sha256")
    args = parser.parse_args()
    if args.report:
        args.report.unlink(missing_ok=True)
    if args.dev and not args.expected_extension_sha256:
        parser.error("--dev requires --expected-extension-sha256")
    if args.wheel and args.expected_extension_sha256:
        parser.error("--expected-extension-sha256 is only valid with --dev")
    import catabolic_native as native

    verification = fixture.wheel_verifier()(args.wheel) if args.wheel else None
    package = Path(native.__file__).resolve()
    assert "site-packages" in package.parts, package
    extension = Path(native._native.__file__).resolve()
    digest = hashlib.sha256(extension.read_bytes()).hexdigest()
    if args.dev:
        assert digest == args.expected_extension_sha256, digest
    suite = unittest.TestSuite(
        InstalledMetadataReview(name)
        for name in sorted(InstalledMetadataReview.__dict__)
        if name.startswith("test_review_")
    )
    result = result_verifier()(
        suite, {InstalledMetadataReview: EXPECTED_TESTS}, expected_count=2
    )
    assert hashlib.sha256(extension.read_bytes()).hexdigest() == digest, (
        "installed extension changed"
    )
    if args.wheel:
        assert fixture.wheel_verifier()(args.wheel) == verification, (
            "installed wheel changed"
        )
    report = {
        "wheel_sha256": hashlib.sha256(args.wheel.read_bytes()).hexdigest()
        if args.wheel
        else None,
        "installed_wheel_verification": verification,
        "development_only": args.dev,
        "script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "fixture_sha256": hashlib.sha256(FIXTURE_PATH.read_bytes()).hexdigest(),
        "result_verifier_sha256": hashlib.sha256(
            Path(__file__).with_name("native_qualification_results.py").read_bytes()
        ).hexdigest(),
        "wheel_verifier_sha256": hashlib.sha256(
            Path(__file__).with_name("qualify_native_wheel.py").read_bytes()
        ).hexdigest(),
        "python": sys.version,
        "installed_package": str(package),
        "extension_sha256": digest,
        "tests": result.testsRun,
        "successful_scenarios": result.successful_scenarios,
        "passed": result.wasSuccessful(),
        "scope": "Independent installed metadata readback cancellation and exception review",
    }
    if args.report:
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    raise SystemExit(not result.wasSuccessful())


if __name__ == "__main__":
    main()
