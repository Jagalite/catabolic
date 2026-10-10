#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Export the pinned Python oracle and retain local baseline results in a new root."""

import argparse
import hashlib
import importlib.metadata
import json
import os
import platform
import shutil
import signal
import sqlite3
import subprocess
import sys
import tarfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REFERENCE = "453fca983222c6665a48775eef67c489c96b527e"

UNIT_RUNNER = r"""import json, sys, unittest
from pathlib import Path
output = Path(sys.argv[1])
def save(result, complete=False):
    temporary = output.with_suffix('.tmp')
    temporary.write_text(json.dumps(dict(
        complete=complete, tests_run=result.testsRun,
        skipped=[dict(test=str(test), reason=reason) for test, reason in result.skipped],
        failures=[dict(test=str(test), traceback=trace) for test, trace in result.failures],
        errors=[dict(test=str(test), traceback=trace) for test, trace in result.errors],
        expected_failures=[dict(test=str(test), traceback=trace) for test, trace in result.expectedFailures],
        unexpected_successes=[str(test) for test in result.unexpectedSuccesses],
    ), indent=2) + '\n')
    temporary.replace(output)
class Result(unittest.TextTestResult):
    def stopTest(self, test):
        super().stopTest(test)
        save(self)
suite = (unittest.defaultTestLoader.loadTestsFromNames(sys.argv[2:])
         if len(sys.argv) > 2 else unittest.defaultTestLoader.discover('tests'))
result = unittest.TextTestRunner(verbosity=2, resultclass=Result).run(suite)
save(result, complete=True)
sys.exit(0 if result.wasSuccessful() else 1)
"""


def write_json(path, data):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
    temporary.replace(path)


def run_check(name, command, source, output, env, timeout):
    started = time.monotonic()
    log = output / f"{name}.log"
    with log.open("w") as stream:
        try:
            completed = subprocess.Popen(
                command,
                cwd=source,
                env=env,
                stdout=stream,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            completed.wait(timeout=timeout)
            status = "passed" if completed.returncode == 0 else "failed"
            code = completed.returncode
        except subprocess.TimeoutExpired:
            try:
                os.killpg(completed.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            completed.wait()
            status, code = "budget-limited", None
        except OSError as error:
            stream.write(str(error) + "\n")
            status, code = "blocked", None
    details = {}
    if name == "unit":
        result_path = output / "unit-result.json"
        if result_path.exists():
            try:
                details = json.loads(result_path.read_text())
            except (ValueError, OSError) as error:
                details = {"complete": False, "report_error": str(error)}
        if status == "passed":
            if not details.get("complete"):
                status = "incomplete"
            elif any(
                details.get(key)
                for key in ("failures", "errors", "unexpected_successes")
            ):
                status = "failed"
            elif details.get("skipped"):
                status = "passed-with-skips"
            elif details.get("expected_failures"):
                status = "passed-with-expected-failures"
    return dict(
        test_result=details,
        name=name,
        command=command,
        status=status,
        exit_code=code,
        elapsed_seconds=round(time.monotonic() - started, 3),
        log=log.name,
        log_sha256=hashlib.sha256(log.read_bytes()).hexdigest(),
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        type=Path,
        required=True,
        help="New directory; existing roots are refused",
    )
    parser.add_argument(
        "--wheel",
        action="store_true",
        help="Build and install exact reference wheel in a new venv",
    )
    parser.add_argument("--checks", action="store_true", help="Run local source checks")
    parser.add_argument(
        "--benchmarks", action="store_true", help="Run synthetic 100/1000/5000 baseline"
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=1800,
        help="Seconds per check; timeout is not a pass",
    )
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    tooling = output / "tooling"
    tooling.mkdir()
    for tool in (Path(__file__), ROOT / "scripts/migration_inventory.py"):
        shutil.copyfile(tool, tooling / tool.name)
    source = output / "source"
    source.mkdir()
    archive = output / "reference.tar"
    subprocess.run(
        ["git", "archive", "--format=tar", f"--output={archive}", REFERENCE],
        cwd=ROOT,
        check=True,
    )
    with tarfile.open(archive) as handle:
        handle.extractall(source, filter="data")
    env = {
        key: val for key, val in os.environ.items() if not key.startswith("CATABOLIC_")
    }
    env["PYTHONPATH"] = str(source / "src")
    env["PYTHONNOUSERSITE"] = "1"
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    env["PATH"] = str(Path(sys.executable).parent) + os.pathsep + env.get("PATH", "")
    # Keep XDG config/cache/data writes within this disposable capture.
    for key, name in (
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_DATA_HOME", "data"),
    ):
        directory = output / name
        directory.mkdir()
        env[key] = str(directory)
    report = dict(
        format_version=1,
        reference_commit=REFERENCE,
        archive_sha256=hashlib.sha256(archive.read_bytes()).hexdigest(),
        runner_sha256=hashlib.sha256(
            (tooling / "migration_baseline.py").read_bytes()
        ).hexdigest(),
        inventory_tool_sha256=hashlib.sha256(
            (tooling / "migration_inventory.py").read_bytes()
        ).hexdigest(),
        environment=dict(
            python=sys.version,
            executable=sys.executable,
            platform=platform.platform(),
            machine=platform.machine(),
            sqlite=sqlite3.sqlite_version,
            packages=sorted(
                (dist.metadata["Name"], dist.version)
                for dist in importlib.metadata.distributions()
            ),
            tools={
                name: shutil.which(name)
                for name in (
                    "ffmpeg",
                    "ffprobe",
                    "pdftotext",
                    "tesseract",
                    "node",
                    "npm",
                    "docker",
                )
            },
        ),
        checks=[],
        milestone_status="in-progress",
        evidence_scope=(
            "local source reference and optional installed-wheel smoke; "
            "not full release or cross-platform qualification"
        ),
    )
    checks = [
        (
            "inventory",
            [
                sys.executable,
                str(tooling / "migration_inventory.py"),
                "--source",
                str(source),
                "--output",
                str(output / "inventory.json"),
            ],
        )
    ]
    if args.wheel:
        installed = output / "installed"
        python = str(installed / "bin/python")
        wheel = output / "dist/catabolic-0.2.0-py3-none-any.whl"
        checks.extend(
            [
                (
                    "wheel-build",
                    [
                        sys.executable,
                        "-m",
                        "pip",
                        "wheel",
                        "--no-deps",
                        "--no-build-isolation",
                        "--wheel-dir",
                        str(output / "dist"),
                        str(source),
                    ],
                ),
                ("wheel-venv", [sys.executable, "-m", "venv", str(installed)]),
                (
                    "wheel-dependencies",
                    [
                        python,
                        "-m",
                        "pip",
                        "install",
                        "--require-hashes",
                        "--only-binary=:all:",
                        "-r",
                        str(source / "requirements/runtime.lock"),
                    ],
                ),
                (
                    "wheel-install",
                    [python, "-m", "pip", "install", "--no-deps", str(wheel)],
                ),
                ("wheel-pip-check", [python, "-m", "pip", "check"]),
                (
                    "wheel-smoke",
                    [
                        python,
                        "-I",
                        "-c",
                        "import catabolic, pathlib, sys; "
                        "p=pathlib.Path(catabolic.__file__).resolve(); "
                        "assert p.is_relative_to(pathlib.Path(sys.prefix)), p; "
                        "print(p); from catabolic.cli import main; "
                        "sys.argv=['catabolic','spec','check']; sys.exit(main())",
                    ],
                ),
            ]
        )
    if args.checks:
        checks.extend(
            [
                (
                    "unit",
                    [
                        sys.executable,
                        "-c",
                        UNIT_RUNNER,
                        str(output / "unit-result.json"),
                    ],
                ),
                (
                    "lint",
                    [sys.executable, "-m", "ruff", "check", "src", "tests", "scripts"],
                ),
                (
                    "format",
                    [
                        sys.executable,
                        "-m",
                        "ruff",
                        "format",
                        "--check",
                        "src",
                        "tests",
                        "scripts",
                    ],
                ),
                ("spec", [sys.executable, "-m", "catabolic", "spec", "check"]),
                (
                    "http-contract",
                    [sys.executable, "scripts/http_contract.py", "--check"],
                ),
                ("docs", [sys.executable, "scripts/sync_docs.py", "--check"]),
            ]
        )
    if args.benchmarks:
        checks.append(
            (
                "synthetic",
                [
                    sys.executable,
                    "-m",
                    "tests.synthetic_benchmark",
                    "--sizes",
                    "100",
                    "1000",
                    "5000",
                    "--root",
                    str(output / "synthetic"),
                ],
            )
        )
    write_json(output / "report.json", report)
    for name, command in checks:
        print(f"Running {name}", flush=True)
        check_env = env.copy()
        if name.startswith("wheel-") and name != "wheel-build":
            check_env.pop("PYTHONPATH", None)
        result = run_check(name, command, source, output, check_env, args.timeout)
        report["checks"].append(result)
        report["artifacts"] = {
            path.relative_to(output).as_posix(): hashlib.sha256(
                path.read_bytes()
            ).hexdigest()
            for path in sorted((output / "dist").glob("*.whl"))
        }
        write_json(output / "report.json", report)
        print(f"{name}: {result['status']}", flush=True)
    if any(check["status"] != "passed" for check in report["checks"]):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
