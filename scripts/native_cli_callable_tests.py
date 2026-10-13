#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Strict installed-wheel qualification of selected returning CLI contracts.

Run outside the source checkout. This never qualifies all command leaves, changes
Python's default entry point, uses development helpers, or touches a real catalog.
"""

import argparse
import ast
import contextlib
import fcntl
import importlib
import importlib.util
import inspect
import io
import json
import os
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch


def clear_stale_report(argv=None, parser=None):
    """Invalidate every canonical --report destination before fallible preflight.

    Only --report PATH and --report=PATH opt in; abbreviations are rejected by
    the full parser. Stop at -- and use that parser's option lexer to distinguish
    filename values (including version-specific negative numbers) from options.
    No argv parsing or project imports happen here. Bootstrap copies are checked
    against this implementation by the receipt-boundary tests.
    """
    if parser is None:
        parser = argparse.ArgumentParser(add_help=False, allow_abbrev=False)
        parser.add_argument("--report")
    arguments = sys.argv[1:] if argv is None else argv
    first_error = None
    for index, argument in enumerate(arguments):
        if argument == "--":
            break
        if argument == "--report":
            value = arguments[index + 1] if index + 1 < len(arguments) else ""
            if parser._parse_optional(value) is not None:
                continue
        elif argument.startswith("--report="):
            value = argument.partition("=")[2]
        else:
            continue
        if value:
            report = Path(value)
            for suffix in (None, ".attempt.json"):
                try:
                    target = report if suffix is None else report.with_suffix(suffix)
                    target.unlink(missing_ok=True)
                except (OSError, ValueError) as error:
                    if first_error is None:
                        first_error = error
    if first_error is not None:
        raise first_error


def argument_parser():
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    for name in (
        "reference-root",
        "reference-python",
        "inventory",
        "source-root",
        "wheel",
        "build-manifest",
        "report",
        "root",
        "oracle-manifest",
    ):
        parser.add_argument("--" + name, type=Path)
    parser.add_argument("--scenario", choices=("reference", "native"))
    parser.add_argument("--oracle-sha256")
    return parser


# Receipt invalidation precedes parsing, project imports, and -O refusal.
if __name__ == "__main__":
    clear_stale_report(parser=argument_parser())

if not __debug__ or os.environ.get("PYTHONOPTIMIZE"):
    raise RuntimeError("callable CLI qualification requires Python assertions")

CONTRACTS = {
    "python:catabolic.cli.main": "Selected returning callable, argv, text streams, exception and process-lifetime contracts",
}
EXPECTED_TESTS = (
    "test_signature",
    "test_parser_system_exit",
    "test_dispatch_returns_zero_and_two",
    "test_argv_none_and_iterables",
    "test_validation_precedes_stdin",
    "test_bounded_stdin_and_eof",
    "test_projection_stdin_validation",
    "test_maintenance_ordered_streams",
    "test_maintenance_shared_stream",
    "test_watch_ordered_streams",
    "test_runtime_error_identity",
    "test_stream_error_rendering",
    "test_handler_output_exception_identity",
    "test_sigint_worker_wait",
    "test_sigint_blocked_stdin",
)
EXPECTED_GROUPS = tuple(name.removeprefix("test_") for name in EXPECTED_TESTS)
REQUIRED_COUNT = 15
MAXIMUM = 1024 * 1024


def sibling(name):
    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).with_name(name + ".py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class RecordingInput(io.StringIO):
    def __init__(self, text):
        super().__init__(text)
        self.reads = []

    def read(self, size=-1):
        assert type(size) is int and 0 < size <= MAXIMUM + 1, size
        self.reads.append(size)
        return super().read(size)


class ForbiddenInput(io.StringIO):
    def read(self, *args):
        raise AssertionError("stdin was read before the command requested it")


class RecordingOutput(io.StringIO):
    def __init__(self, name, events):
        super().__init__()
        self.name, self.events = name, events

    def write(self, text):
        self.events.append([self.name, "write", text])
        return super().write(text)

    def flush(self):
        self.events.append([self.name, "flush"])
        return super().flush()


def ordered_segments(events):
    """Coalesce print's text/newline writes, preserving cross-stream ordering."""
    result = []
    for name, operation, *value in events:
        if operation != "write" or not value[0]:
            continue
        if result and result[-1][0] == name:
            result[-1][1] += value[0]
        else:
            result.append([name, value[0]])
    return result


class CallableCases(unittest.TestCase):
    engine = None
    cli = None
    root = None
    groups = None
    launches = None
    calls = 0
    known_gaps = None

    def setUp(self):
        self.directory = self.root / self._testMethodName
        self.directory.mkdir()
        self.database = self.directory / "fixture.db"
        self.start_launches = len(self.launches)
        self.previous_alarm = signal.getsignal(signal.SIGALRM)

        def expired(signum, frame):
            raise RuntimeError("callable qualification scenario exceeded 45 seconds")

        signal.signal(signal.SIGALRM, expired)
        signal.alarm(45)

    def tearDown(self):
        try:
            for child in self.launches[self.start_launches :]:
                self.assertIsNotNone(
                    child.returncode, "native child was not waited/reaped"
                )
                self.assertTrue(child.stdin.closed)
                self.assertTrue(child.stdout.closed)
                with self.assertRaises(ChildProcessError):
                    os.waitpid(child.pid, os.WNOHANG)
        finally:
            signal.alarm(0)
            signal.signal(signal.SIGALRM, self.previous_alarm)
            # A failed qualification must not strand the child it discovered.
            for child in self.launches[self.start_launches :]:
                if child.poll() is None:
                    child.kill()
                child.wait()
                for stream in (child.stdin, child.stdout):
                    stream.close()

    def save(self, value):
        key = self._testMethodName.removeprefix("test_")
        self.assertNotIn(key, self.groups)
        # Paths identify disposable fixture roots only. No output bytes or
        # error classes are hidden, and there is no timing/random-value scrub.
        self.groups[key] = json.loads(
            json.dumps(value, ensure_ascii=True).replace(str(self.directory), "<ROOT>")
        )

    def call(self, arguments, *, stdin=None, stdout=None, stderr=None, expected=None):
        stdout = io.StringIO() if stdout is None else stdout
        stderr = io.StringIO() if stderr is None else stderr
        stdin = ForbiddenInput() if stdin is None else stdin
        type(self).calls += 1
        with (
            patch.object(sys, "stdin", stdin),
            contextlib.redirect_stdout(stdout),
            contextlib.redirect_stderr(stderr),
        ):
            try:
                value = self.cli.main(arguments)
                result = {"return": value}
            except BaseException as error:
                if expected is not None:
                    self.assertIs(
                        error, expected, "callback exception identity changed"
                    )
                result = {"exception": type(error).__name__, "message": str(error)}
                if isinstance(error, SystemExit):
                    result["code"] = error.code
        if expected is not None:
            self.assertEqual(result.get("exception"), type(expected).__name__)
            result["same_exception"] = True
        result.update(stdout=stdout.getvalue(), stderr=stderr.getvalue())
        return result

    def fixture(self, *, source=False):
        result = self.call(["--db", str(self.database), "init"])
        self.assertEqual(result.get("return"), 0, result)
        if source:
            directory = self.directory / "empty-source"
            directory.mkdir()
            result = self.call(
                [
                    "--db",
                    str(self.database),
                    "location",
                    "bind",
                    "fixture",
                    "--root",
                    str(directory),
                ]
            )
            self.assertEqual(result.get("return"), 0, result)
        return ["--db", str(self.database)]

    def assert_locks_released(self):
        for suffix in (".lock", ".api-worker.lock", ".plex-metadata.lock"):
            path = Path(str(self.database) + suffix)
            if path.exists():
                with path.open("rb+") as stream:
                    fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    fcntl.flock(stream, fcntl.LOCK_UN)
        with sqlite3.connect(
            self.database.as_uri() + "?mode=rw", uri=True, timeout=0
        ) as db:
            db.execute("BEGIN IMMEDIATE")
            db.rollback()
            self.assertEqual(
                db.execute("SELECT count(*) FROM api_worker_status").fetchone()[0], 0
            )

    def test_signature(self):
        signature = inspect.signature(self.cli.main, eval_str=True)
        self.assertEqual(str(signature), "(argv: list[str] | None = None) -> int")
        self.save(str(signature))

    def test_parser_system_exit(self):
        results = []
        for arguments, code in (
            (["--help"], 0),
            (["--version"], 0),
            (["not-a-command"], 2),
            ([], 2),
        ):
            result = self.call(arguments)
            self.assertEqual(result.get("exception"), "SystemExit", result)
            self.assertEqual(result.get("code"), code, result)
            results.append(result)
        self.save(results)

    def test_dispatch_returns_zero_and_two(self):
        success = self.call(["--json", "item", "types"])
        error = self.call(["status"])
        self.assertEqual(success.get("return"), 0, success)
        self.assertEqual(error.get("return"), 2, error)
        self.save([success, error])

    def test_argv_none_and_iterables(self):
        with patch.object(sys, "argv", ["embedded-host", "--version"]):
            implicit = self.call(None)
        explicit = self.call(("--version",))
        generator = self.call(iter(["--version"]))
        self.assertEqual(implicit, explicit)
        self.assertEqual(explicit, generator)
        self.save([implicit, explicit, generator])

    def test_validation_precedes_stdin(self):
        prefix = self.fixture()
        results = []
        cases = (
            ["--help", "--file", "-"],
            ["query", "--file", "-"],
            prefix + ["query", "SELECT 1", "--file", "-"],
            prefix + ["graphql", "{items{nodes{id}}}", "--file", "-"],
            ["projection", "mapping-preview", "--definition", "-"],
            prefix + ["projection", "mapping-apply", "--definition", "-"],
        )
        for arguments in cases:
            result = self.call(arguments)
            self.assertNotEqual(result.get("exception"), "AssertionError", result)
            results.append(result)
        self.save(results)

    def test_bounded_stdin_and_eof(self):
        prefix = self.fixture() + ["--json", "query", "--file", "-"]
        results = []
        for text, code in (("SELECT 'é𝄞' AS unicode, 7 AS answer", 0), ("", 2)):
            stdin = RecordingInput(text)
            result = self.call(prefix, stdin=stdin)
            self.assertEqual(result.get("return"), code, result)
            self.assertEqual(stdin.reads, [MAXIMUM + 1])
            self.assertEqual(stdin.tell(), min(len(text), MAXIMUM + 1))
            results.append(
                {"result": result, "reads": stdin.reads, "position": stdin.tell()}
            )
        self.save(results)
        # Known pre-existing leaf diagnostic gap: retain exact negative evidence
        # separately, never normalize different messages into a parity pass.
        text = "x" * (MAXIMUM + 2)
        stdin = RecordingInput(text)
        oversized = self.call(prefix, stdin=stdin)
        self.assertEqual(oversized.get("return"), 2, oversized)
        self.assertEqual(stdin.reads, [MAXIMUM + 1])
        self.assertEqual(stdin.tell(), MAXIMUM + 1)
        expected = (
            "SQL exceeds the 1 MiB input limit"
            if self.engine == "reference"
            else "input exceeds 1048576 bytes"
        )
        self.assertIn(expected, oversized["stderr"])
        if self.engine == "native":
            direct = subprocess.run(
                [str(self.cli.legacy_binary()), *prefix],
                input=text,
                capture_output=True,
                text=True,
                timeout=20,
            )
            self.assertEqual(
                {
                    "return": direct.returncode,
                    "stdout": direct.stdout,
                    "stderr": direct.stderr,
                },
                oversized,
            )
        self.known_gaps["oversized_sql_diagnostic"] = {
            "result": oversized,
            "read_sizes": stdin.reads,
            "standalone_native_equal": self.engine == "native",
        }

    def test_projection_stdin_validation(self):
        prefix = self.fixture() + ["--json", "projection"]
        results = []
        for operation in ("mapping-preview", "mapping-apply"):
            arguments = prefix + [operation, "--definition", "-"]
            if operation == "mapping-apply":
                arguments += ["--expected-plan", "fixture-unaccepted-plan"]
            stdin = RecordingInput('{"version":3}')
            result = self.call(arguments, stdin=stdin)
            self.assertEqual(result.get("return"), 2, result)
            self.assertIn("mapping_invalid_http_definition", result["stderr"])
            self.assertEqual(stdin.reads, [MAXIMUM + 1])
            results.append({"result": result, "reads": stdin.reads})
        self.assert_locks_released()
        self.save(results)

    def maintenance(self, *, shared=False):
        prefix = self.fixture(source=True)
        events = []
        stdout = RecordingOutput("stdout", events)
        stderr = stdout if shared else RecordingOutput("stderr", events)
        result = self.call(
            prefix + ["maintenance", "--inventory-only"], stdout=stdout, stderr=stderr
        )
        self.assertEqual(result.get("return"), 0, result)
        segments = ordered_segments(events)
        self.assertTrue(any(event[1] == "flush" for event in events))
        self.assertIn('"stage": "preflight"', stderr.getvalue())
        self.assertIn('"stage": "scan"', stderr.getvalue())
        self.assertIn("Maintenance cycle: complete", stdout.getvalue())
        if shared:
            self.assertLess(
                stdout.getvalue().index('"stage": "scan"'),
                stdout.getvalue().index("Maintenance cycle: complete"),
            )
        else:
            self.assertEqual([segment[0] for segment in segments], ["stderr", "stdout"])
            self.assertEqual(segments[0][1], stderr.getvalue())
            self.assertEqual(segments[1][1], stdout.getvalue())
        self.assertEqual(list((self.directory / "empty-source").iterdir()), [])
        self.assertFalse((self.directory / "output").exists())
        self.assert_locks_released()
        self.save({"result": result, "ordered_segments": segments})

    def test_maintenance_ordered_streams(self):
        self.maintenance()

    def test_maintenance_shared_stream(self):
        self.maintenance(shared=True)

    def test_watch_ordered_streams(self):
        # Empty inventory keeps this a transport witness; nonempty delegated
        # scan report parity remains separately incomplete.
        prefix = self.fixture()
        results = []
        for mode in ([], ["--json"], ["--machine"]):
            events = []
            stdout = RecordingOutput("stdout", events)
            stderr = RecordingOutput("stderr", events)
            result = self.call(
                prefix + mode + ["watch", "--kind", "sniff"],
                stdout=stdout,
                stderr=stderr,
            )
            self.assertEqual(result.get("return"), 0, result)
            self.assertEqual(len(stderr.getvalue().splitlines()), 1)
            progress = json.loads(stderr.getvalue())
            self.assertEqual(progress["cycle"], 1)
            self.assertEqual(progress["scan"]["scans"], [])
            segments = ordered_segments(events)
            self.assertEqual([segment[0] for segment in segments], ["stderr", "stdout"])
            results.append({"result": result, "ordered_output": segments})
        self.assert_locks_released()
        self.save(results)

    def test_runtime_error_identity(self):
        prefix = self.fixture(source=True)
        results = []
        for channel, method in (
            ("stdout", "write"),
            ("stderr", "write"),
            ("stderr", "flush"),
            ("stdin", "read"),
        ):
            sentinel = RuntimeError("fixture callback failure")

            class Broken(io.StringIO):
                def write(self, text, error=sentinel, at=method):
                    if at == "write":
                        raise error
                    return super().write(text)

                def read(self, size=-1, error=sentinel):
                    raise error

                def flush(self, error=sentinel, at=method):
                    if at == "flush":
                        raise error
                    return super().flush()

            arguments = prefix + (
                ["query", "--file", "-"]
                if channel == "stdin"
                else ["--json", "maintenance", "--inventory-only"]
                if channel == "stderr"
                else ["--json", "item", "types"]
            )
            result = self.call(arguments, expected=sentinel, **{channel: Broken()})
            self.assert_locks_released()
            results.append(result)
        self.save(results)

    def test_stream_error_rendering(self):
        prefix = self.fixture(source=True)
        results = []
        for kind in (
            OSError,
            ValueError,
            sqlite3.OperationalError,
            self.cli.CatabolicError,
        ):
            for machine in (False, True):
                for channel, method in (
                    ("stdin", "read"),
                    ("stdout", "write"),
                    ("stderr", "write"),
                    ("stderr", "flush"),
                ):
                    sentinel = kind("fixture text stream failure")

                    class FailOnce(io.StringIO):
                        failed = False

                        def fail(self, error=sentinel):
                            if not self.failed:
                                self.failed = True
                                raise error

                        def read(self, size=-1, at=method):
                            if at == "read":
                                self.fail()
                            return super().read(size)

                        def write(self, text, at=method):
                            if at == "write":
                                self.fail()
                            return super().write(text)

                        def flush(self, at=method):
                            if at == "flush":
                                self.fail()
                            return super().flush()

                    arguments = prefix + (["--machine"] if machine else ["--json"])
                    arguments += (
                        ["query", "--file", "-"]
                        if channel == "stdin"
                        else ["maintenance", "--inventory-only"]
                        if channel == "stderr"
                        else ["item", "types"]
                    )
                    result = self.call(arguments, **{channel: FailOnce()})
                    self.assertEqual(
                        result.get("return"), 3 if channel == "stderr" else 2, result
                    )
                    self.assertIn(kind.__name__, result["stdout"] + result["stderr"])
                    self.assert_locks_released()
                    results.append(result)
        self.save(results)

    def test_handler_output_exception_identity(self):
        results = []
        for kind in (
            RuntimeError,
            OSError,
            ValueError,
            sqlite3.OperationalError,
            self.cli.CatabolicError,
            KeyboardInterrupt,
        ):
            for flag in (None, "--json", "--machine"):
                sentinel = kind("fixture handler output failure")

                class Broken(io.StringIO):
                    def write(self, text, error=sentinel):
                        raise error

                arguments = ([flag] if flag else []) + ["status"]
                channel = "stdout" if flag == "--machine" else "stderr"
                results.append(
                    self.call(arguments, expected=sentinel, **{channel: Broken()})
                )
        # A failure while reporting an initial stdout callback failure is already
        # inside frozen main's except-handler, including the native render-only
        # request. Its exact second exception must escape unchanged.
        for kind in (
            RuntimeError,
            OSError,
            ValueError,
            sqlite3.OperationalError,
            self.cli.CatabolicError,
            KeyboardInterrupt,
        ):
            for flag in (None, "--json", "--machine"):
                sentinel = kind("fixture second renderer failure")

                class Initial(io.StringIO):
                    writes = 0

                    def write(self, text, error=sentinel):
                        self.writes += 1
                        if self.writes == 1:
                            raise OSError("fixture initial success-output failure")
                        raise error

                class Handler(io.StringIO):
                    def write(self, text, error=sentinel):
                        raise error

                arguments = ([flag] if flag else []) + ["item", "types"]
                results.append(
                    self.call(
                        arguments, stdout=Initial(), stderr=Handler(), expected=sentinel
                    )
                )
        self.save(results)

    def test_sigint_worker_wait(self):
        prefix = self.fixture()
        done = threading.Event()
        ready = threading.Event()
        errors = []

        def interrupt_when_waiting():
            deadline = time.monotonic() + 20
            try:
                while not done.is_set() and time.monotonic() < deadline:
                    with sqlite3.connect(
                        self.database.as_uri() + "?mode=ro", uri=True, timeout=0.1
                    ) as db:
                        row = db.execute(
                            "SELECT worker_pid FROM api_worker_status"
                        ).fetchone()
                    if row:
                        # Heartbeat proves dispatch/ownership. Allow the empty
                        # tick to enter its real two-second native wait.
                        if done.wait(0.2):
                            return
                        ready.set()
                        os.kill(os.getpid(), signal.SIGINT)  # host PID only
                        return
                    done.wait(0.02)
                if not done.is_set():
                    errors.append("API worker did not reach its heartbeat")
                    os.kill(os.getpid(), signal.SIGINT)
            except BaseException as error:
                errors.append(repr(error))
                os.kill(os.getpid(), signal.SIGINT)

        thread = threading.Thread(
            target=interrupt_when_waiting, name="callable-sigint-witness"
        )
        thread.start()
        try:
            result = self.call(prefix + ["--machine", "api", "worker"])
        finally:
            done.set()
            thread.join(timeout=5)
        self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])
        self.assertTrue(ready.is_set())
        self.assertEqual(result.get("return"), 130, result)
        self.assert_locks_released()
        self.save({"result": result, "host_only_sigint": True, "locks_released": True})

    def test_sigint_blocked_stdin(self):
        prefix = self.fixture()
        results = []
        for operation in ("query", "mapping-preview", "mapping-apply"):
            read_fd, write_fd = os.pipe()
            entered = threading.Event()
            done = threading.Event()
            stdin = os.fdopen(read_fd, "r", encoding="utf-8")

            class Blocked:
                reads = []

                def read(self, size, _entered=entered, _stdin=stdin):
                    self.reads.append(size)
                    _entered.set()
                    return _stdin.read(size)

            blocked = Blocked()

            def interrupt_read(_entered=entered, _done=done):
                if _entered.wait(10) and not _done.wait(0.1):
                    os.kill(os.getpid(), signal.SIGINT)  # never process group

            thread = threading.Thread(
                target=interrupt_read, name="stdin-sigint-witness"
            )
            thread.start()
            arguments = prefix + ["--machine"]
            arguments += (
                ["query", "--file", "-"]
                if operation == "query"
                else ["projection", operation, "--definition", "-"]
            )
            if operation == "mapping-apply":
                arguments += ["--expected-plan", "fixture-unaccepted-plan"]
            try:
                result = self.call(arguments, stdin=blocked)
            finally:
                done.set()
                os.close(write_fd)
                stdin.close()
                thread.join(timeout=12)
            self.assertFalse(thread.is_alive())
            self.assertTrue(entered.is_set())
            self.assertEqual(blocked.reads, [MAXIMUM + 1])
            self.assertEqual(result.get("return"), 130, result)
            self.assert_locks_released()
            results.append(
                {"result": result, "reads": blocked.reads, "host_only_sigint": True}
            )
        self.save(results)


def run_scenario(args):
    qualification = sibling("native_facade_qualification")
    provenance = sibling("qualify_native_embedding")
    assert args.root and args.oracle_manifest and args.oracle_sha256
    assert qualification.digest(args.oracle_manifest) == args.oracle_sha256
    oracle = json.loads(args.oracle_manifest.read_text())["oracle"]
    assert oracle["head"] == qualification.FROZEN_COMMIT
    assert Path(oracle["root"]).resolve() == args.reference_root.resolve()
    if args.scenario == "reference":
        assert list(sys.version_info[:3]) == qualification.REFERENCE_VERSION
        assert (
            provenance.reference_provenance(
                args.reference_root, qualification.FROZEN_COMMIT
            )
            == oracle
        )
        sys.path.insert(0, str(args.reference_root / "src"))
    namespace = "catabolic" if args.scenario == "reference" else "catabolic_native"
    cli = importlib.import_module(namespace + ".cli")
    assert (
        "catabolic_native" if namespace == "catabolic" else "catabolic"
    ) not in sys.modules
    root = args.root / args.scenario
    root.mkdir()
    launches = []
    actual_popen = subprocess.Popen
    helper = cli.legacy_binary() if args.scenario == "native" else None

    def observe_popen(arguments, *positional, **keywords):
        child = actual_popen(arguments, *positional, **keywords)
        if (
            helper is not None
            and str(arguments[0]) == str(helper)
            and arguments[1:] == ["__catabolic-python-cli-v1"]
        ):
            assert arguments == [str(helper), "__catabolic-python-cli-v1"]
            assert keywords.get("start_new_session") is True
            launches.append(child)
        return child

    CallableCases.engine, CallableCases.cli, CallableCases.root = (
        args.scenario,
        cli,
        root,
    )
    CallableCases.groups, CallableCases.launches, CallableCases.calls = {}, launches, 0
    CallableCases.known_gaps = {}
    environment = {
        "HOME": str(root),
        "COLUMNS": "80",
        "TERM": "dumb",
        "PATH": str(root / "empty-bin"),
    }
    with (
        patch.dict(os.environ, environment),
        patch.object(subprocess, "Popen", observe_popen),
    ):
        for key in ("CATABOLIC_DB", "CATABOLIC_PROFILE"):
            os.environ.pop(key, None)
        result = sibling("native_qualification_results").run_required_suite(
            unittest.defaultTestLoader.loadTestsFromTestCase(CallableCases),
            {CallableCases: EXPECTED_TESTS},
            REQUIRED_COUNT,
        )
    assert set(CallableCases.groups) == set(EXPECTED_GROUPS)
    assert CallableCases.calls > 0
    if args.scenario == "native":
        assert len(launches) >= CallableCases.calls
        assert cli.legacy_binary() == helper
    imports = None
    if args.scenario == "reference":
        imports = provenance.reference_imports(
            args.reference_root, oracle["tracked_source_sha256"]
        )
        assert (
            provenance.reference_provenance(
                args.reference_root, qualification.FROZEN_COMMIT
            )
            == oracle
        )
    assert qualification.digest(args.oracle_manifest) == args.oracle_sha256
    return dict(
        groups=CallableCases.groups,
        known_gaps=CallableCases.known_gaps,
        calls=CallableCases.calls,
        tests=result.testsRun,
        successful_scenarios=result.successful_scenarios,
        launched_children=len(launches),
        oracle_imports=imports,
    )


def companion_launcher(source_root):
    """Check source and execute only inside a disposable subprocess host."""
    import catabolic_native as native

    source = inspect.getsource(native.main)
    tree = ast.parse(source)
    assert any(
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and isinstance(node.func.value, ast.Name)
        and node.func.value.id == "os"
        and node.func.attr == "execv"
        for node in ast.walk(tree)
    )
    run = subprocess.run(
        [
            sys.executable,
            "-I",
            "-B",
            "-c",
            "import catabolic_native; catabolic_native.main()",
            "--version",
        ],
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert run.returncode == 0, (run.stdout, run.stderr)
    assert native.__version__ in run.stdout
    assert (
        'catabolic = "catabolic.cli:main"'
        in (source_root / "pyproject.toml").read_text()
    )
    return {
        "exec_launcher_in_subprocess": True,
        "version_output": run.stdout,
        "python_default_preserved": True,
    }


def main():
    parser = argument_parser()
    clear_stale_report(parser=parser)
    args = parser.parse_args()
    for name, value in vars(args).items():
        if isinstance(value, Path):
            setattr(args, name, value.absolute())
    if args.scenario:
        print(json.dumps(run_scenario(args)))
        return
    required = (
        "reference_root",
        "reference_python",
        "inventory",
        "source_root",
        "wheel",
        "build_manifest",
        "report",
    )
    if not all(getattr(args, name) for name in required):
        parser.error(
            "strict gate requires "
            + ", ".join("--" + name.replace("_", "-") for name in required)
        )
    assert not any(
        key in os.environ
        for key in (
            "CATABOLIC_NATIVE_BINARY",
            "CATABOLIC_LEGACY_BINARY",
            "CATABOLIC_LEGACY_NATIVE_BINARY",
        )
    ), "helper overrides are forbidden"
    qualification = sibling("native_facade_qualification")
    provenance = sibling("qualify_native_embedding")
    inventory = json.loads(args.inventory.read_text())
    rows = qualification.inventory_rows(inventory, CONTRACTS, 1)
    assert rows[0]["signature"]["arguments"] == "argv: list[str] | None=None"
    assert rows[0]["signature"]["returns"] == "int"
    identity = qualification.NativeIdentity(args.source_root)
    fence = qualification.file_fence(__file__, args.inventory, args.build_manifest)
    fence["default_entrypoint_sha256"] = qualification.digest(
        args.source_root / "pyproject.toml"
    )
    runtime = qualification.reference_runtime(args.reference_python)
    native_runtime = {
        "version": list(sys.version_info[:3]),
        "executable": sys.executable,
        "executable_sha256": qualification.digest(sys.executable),
    }
    artifact = qualification.strict_artifact(
        args.wheel, args.build_manifest, args.source_root
    )
    source = sibling("build_native_wheel").source_digest(args.source_root)
    oracle = provenance.reference_provenance(
        args.reference_root, qualification.FROZEN_COMMIT
    )
    with tempfile.TemporaryDirectory(
        prefix="catabolic-callable-installed-"
    ) as directory:
        root = Path(directory)
        oracle_manifest = root / "oracle.json"
        oracle_manifest.write_text(json.dumps({"oracle": oracle}))
        outputs = {}
        for engine, interpreter in (
            ("reference", args.reference_python),
            ("native", Path(sys.executable)),
        ):
            command = [
                str(interpreter),
                "-I",
                "-B",
                str(Path(__file__).resolve()),
                "--scenario",
                engine,
                "--root",
                directory,
                "--reference-root",
                str(args.reference_root),
                "--oracle-manifest",
                str(oracle_manifest),
                "--oracle-sha256",
                qualification.digest(oracle_manifest),
            ]
            completed = subprocess.run(
                command, cwd=root, capture_output=True, text=True, timeout=150
            )
            assert completed.returncode == 0, (
                engine,
                completed.stdout,
                completed.stderr,
            )
            outputs[engine] = json.loads(completed.stdout)
            assert outputs[engine]["tests"] == REQUIRED_COUNT
            assert len(outputs[engine]["successful_scenarios"]) == REQUIRED_COUNT
        qualification.paired_groups(
            outputs["reference"]["groups"],
            outputs["native"]["groups"],
            EXPECTED_GROUPS,
            REQUIRED_COUNT,
        )
        assert (
            set(outputs["reference"]["known_gaps"])
            == set(outputs["native"]["known_gaps"])
            == {"oversized_sql_diagnostic"}
        )
        assert (
            outputs["reference"]["known_gaps"]["oversized_sql_diagnostic"]["result"]
            != outputs["native"]["known_gaps"]["oversized_sql_diagnostic"]["result"]
        )
        assert (
            outputs["native"]["known_gaps"]["oversized_sql_diagnostic"][
                "standalone_native_equal"
            ]
            is True
        )
        companion = companion_launcher(args.source_root)
    identity.verify()
    assert (
        qualification.strict_artifact(args.wheel, args.build_manifest, args.source_root)
        == artifact
    )
    assert sibling("build_native_wheel").source_digest(args.source_root) == source
    repeated = qualification.file_fence(__file__, args.inventory, args.build_manifest)
    repeated["default_entrypoint_sha256"] = qualification.digest(
        args.source_root / "pyproject.toml"
    )
    assert repeated == fence
    assert qualification.reference_runtime(args.reference_python) == runtime
    assert qualification.digest(sys.executable) == native_runtime["executable_sha256"]
    assert (
        provenance.reference_provenance(
            args.reference_root, qualification.FROZEN_COMMIT
        )
        == oracle
    )
    report = dict(
        format=1,
        passed=True,
        development_only=False,
        scope="Selected installed returning CLI callable contracts only",
        tests=REQUIRED_COUNT,
        paired_groups=REQUIRED_COUNT,
        paired_group_names=list(EXPECTED_GROUPS),
        successful_scenarios=outputs["native"]["successful_scenarios"],
        coverage=[
            dict(
                id=rows[0]["id"],
                installed_calls=outputs["native"]["calls"],
                paired_calls=outputs["reference"]["calls"],
                selected_contracts=CONTRACTS[rows[0]["id"]],
                qualification="selected-contracts-tested-with-gaps",
            )
        ],
        native_children_reaped=outputs["native"]["launched_children"],
        known_gaps={engine: output["known_gaps"] for engine, output in outputs.items()},
        companion=companion,
        artifact=artifact,
        oracle=oracle,
        oracle_imports=outputs["reference"]["oracle_imports"],
        reference_runtime=runtime,
        native_runtime=native_runtime,
        **fence,
        limitations=[
            "Oversized SQL stdin preserves the native standalone generic input-limit diagnostic, which differs from the frozen SQL-specific message; this is an explicit negative witness, not paired parity",
            "All CLI leaves remain partial/unqualified; selected call-boundary parity is not full command or release qualification",
            "Python remains the default; no automatic fallback, managed output publication, real catalog, live network/service or credentials were used",
            "Synchronous main-thread text streams and Linux process/signal lifecycle only; concurrent calls, alternate event loops and pathological custom streams are not qualified",
        ],
    )
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            {
                "tests": REQUIRED_COUNT,
                "paired_groups": REQUIRED_COUNT,
                "inventory_rows": 1,
                "report": str(args.report),
            }
        )
    )


if __name__ == "__main__":
    main()
