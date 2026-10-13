# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Fail-closed source contract for the repository's literal installed CI block.

This deliberately accepts only the current literal YAML/shell form. It is not a
YAML or shell interpreter, nor evidence that GitHub has executed the workflow.
"""

import re

# Deliberately fixed metadata: unsupported YAML forms must fail closed.
WORKFLOW_PREFIX = "# SPDX-FileCopyrightText: 2026 The Catabolic Contributors\n# SPDX-License-Identifier: MIT\n\nname: Opt-in native Linux checkpoint\non:\n  workflow_dispatch:\n  pull_request:\n    paths:\n      - 'crates/**'\n      - 'src/**'\n      - 'requirements/**'\n      - 'pyproject.toml'\n      - 'Cargo.*'\n      - 'rust-toolchain.toml'\n      - '.cargo/**'\n      - 'scripts/**'\n      - 'tests/**'\n      - '.github/workflows/**'\npermissions:\n  contents: read\nenv:\n  CARGO_INCREMENTAL: '0'\n  CARGO_PROFILE_DEV_DEBUG: '0'\n  CARGO_PROFILE_TEST_DEBUG: '0'\njobs:\n"
INSTALLED_HEADER = "    needs: native\n    runs-on: ubuntu-24.04\n    timeout-minutes: 45\n    strategy:\n      fail-fast: false\n      matrix:\n        python: ['3.11', '3.14.6']\n    steps:\n"

STEP = "Outside-checkout installed helper, cancellation and persistent facade"
UPLOAD = "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02 # v4"
PREFIX = (
    'root="$PWD"',
    'reference_python="${{ steps.oracle-python.outputs.python-path }}"',
    'wheel="$(find "$root/dist/native" -maxdepth 1 -name \'*.whl\' -print)"',
    'cd "$RUNNER_TEMP"',
)


def require_installed_gate(workflow, script, report, extra=()):
    """Require an active strict command and its actual always-upload receipt."""
    _require(
        workflow.startswith(WORKFLOW_PREFIX),
        "unsupported workflow metadata or shell defaults",
    )
    _require(
        all(
            not line or line[0].isspace() or line.startswith("#")
            for line in workflow[len(WORKFLOW_PREFIX) :].splitlines()
        ),
        "unsupported later top-level workflow metadata",
    )
    jobs = re.findall(r"^  installed:\n([\s\S]*?)(?=^  \S|\Z)", workflow, re.MULTILINE)
    _require(len(jobs) == 1, "expected one installed job")
    job = jobs[0]
    _require(
        job.startswith(INSTALLED_HEADER),
        "unsupported installed-job metadata or shell defaults",
    )
    _require(
        not re.search(r"^    \S", job[len(INSTALLED_HEADER) :], re.MULTILINE),
        "unexpected job-level metadata after installed steps",
    )
    blocks = re.findall(
        "^      - name: "
        + re.escape(STEP)
        + "\\n        run: \\|\\n((?:^          .*\\n|^\\n)*)(?=^      -|\\Z)",
        job,
        re.MULTILINE,
    )
    _require(len(blocks) == 1, "expected one unconditional literal installed step")
    lines = [line[10:] for line in blocks[0].splitlines() if line.strip()]
    _require(
        tuple(lines[:4]) == PREFIX, "installed setup or outside-checkout cwd changed"
    )
    for line in lines[4:]:
        if line.startswith("#") or line in (
            "for surface in import matching metadata; do",
            "done",
        ):
            continue
        _require(
            line.lstrip().startswith('"$RUNNER_TEMP/native-installed/bin/python" ')
            and not any(
                token in line for token in (";", "|", "&", "<", ">", "`", "\\")
            ),
            "installed step contains unsupported shell control or failure suppression",
        )
    command = (
        '"$RUNNER_TEMP/native-installed/bin/python" -I "$root/scripts/'
        + script
        + '" --wheel "$wheel" --source-root "$root" --build-manifest "$root/dist/native/build-manifest.json"'
    )
    command += "".join((" " + item for item in extra))
    command += (
        ' --inventory "$root/tests/parity/python-api.json" --reference-root "$root/.local-tests/rust-migration/reference" --reference-python "$reference_python" --report "$RUNNER_TEMP/'
        + report
        + '.json"'
    )
    selected = [line for line in lines if script in line]
    _require(
        selected == [command],
        "strict installed command missing, duplicated, or changed",
    )
    _require(
        sum((script in line for line in workflow.splitlines())) == 1,
        "gate must occur exactly once in workflow",
    )
    loops = [
        i
        for i, line in enumerate(lines)
        if line == "for surface in import matching metadata; do"
    ]
    _require(
        len(loops) == 1 and loops[0] > lines.index(command),
        "gate must run before the only allowed helper loop",
    )
    _require(lines.count("done") == 1, "unexpected loop structure")
    uploads = re.findall(
        "^      - uses: "
        + re.escape(UPLOAD)
        + "\\n        if: always\\(\\)\\n        with:\\n          name: opt-in-native-installed-\\$\\{\\{ matrix.python \\}\\}\\n          path: \\|\\n([\\s\\S]*)\\Z",
        job,
        re.MULTILINE,
    )
    _require(
        len(uploads) == 1, "expected one actual always-upload installed receipt step"
    )
    _require(
        all(
            re.fullmatch(
                r"            \$\{\{ runner.temp \}\}/native-[a-z0-9.*-]+\.json", line
            )
            for line in uploads[0].splitlines()
            if line.strip() and not line.startswith("#")
        ),
        "unsupported upload path syntax or receipt exclusion",
    )
    receipt = "            ${{ runner.temp }}/" + report + "*.json"
    _require(
        uploads[0].splitlines().count(receipt) == 1,
        "receipt upload path missing or changed",
    )


def _require(condition, message):
    if not condition:
        raise AssertionError(message)
