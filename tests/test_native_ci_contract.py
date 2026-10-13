# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Mutation controls for the literal installed CI source contract."""

import unittest
from pathlib import Path

from tests.native_ci_contract import STEP, UPLOAD, require_installed_gate

ROOT = Path(__file__).resolve().parents[1]


class InstalledCIContract(unittest.TestCase):
    def test_exact_workflow_and_disabled_or_weakened_mutations(self):
        workflow = (ROOT / ".github/workflows/native-qualification.yml").read_text()
        for surface, script, report in (
            (
                "publisher",
                "native_publisher_embedding_tests.py",
                "native-publisher-embedding",
            ),
            ("admin", "native_access_admin_tests.py", "native-access-admin"),
            ("legacy", "native_legacy_adapter_tests.py", "native-legacy-adapters"),
            ("layout", "native_layout_embedding_tests.py", "native-layout-embedding"),
            (
                "reconciler",
                "native_reconciler_embedding_tests.py",
                "native-reconciler-embedding",
            ),
            (
                "watchers",
                "native_watchers_embedding_tests.py",
                "native-watchers-embedding",
            ),
        ):
            extra = (
                (
                    '--native-binary "$root/dist/native/catabolic-native"',
                    '--legacy-binary "$root/dist/native/catabolic-legacy-native"',
                )
                if surface in {"admin", "legacy"}
                else ()
            )
            require_installed_gate(workflow, script, report, extra)
            command = next(line for line in workflow.splitlines() if script in line)
            upload = "            ${{ runner.temp }}/" + report + "*.json"
            timeout = "    timeout-minutes: 45\n"
            self.assertEqual(workflow.count(timeout), 1)
            mutations = {
                "unpinned_python": workflow.replace(
                    "python: ['3.11', '3.14.6']", "python: ['3.11', '3.14']"
                ),
                "wrong_python_patch": workflow.replace(
                    "python: ['3.11', '3.14.6']", "python: ['3.11', '3.14.5']"
                ),
                "comment": workflow.replace(command, "          # " + command.strip()),
                "shell_noop": workflow.replace(
                    command, "          : " + command.strip()
                ),
                "ignore_failure": workflow.replace(command, command + " || true"),
                "delete": workflow.replace(command, ""),
                "duplicate": workflow.replace(command, command + "\n" + command),
                "wrong_interpreter": workflow.replace(
                    command,
                    command.replace("native-installed/bin/python", "other/bin/python"),
                ),
                "wrong_wheel": workflow.replace(
                    command, command.replace('--wheel "$wheel"', '--wheel "wrong.whl"')
                ),
                "missing_isolation": workflow.replace(
                    command, command.replace(" -I ", " ")
                ),
                "wrong_source": workflow.replace(
                    command,
                    command.replace(
                        '--source-root "$root"', '--source-root "elsewhere"'
                    ),
                ),
                "missing_manifest": workflow.replace(
                    command,
                    command.replace(
                        ' --build-manifest "$root/dist/native/build-manifest.json"', ""
                    ),
                ),
                "dev_wheel": workflow.replace(
                    command, command.replace("--wheel", "--dev-wheel")
                ),
                "missing_cd": workflow.replace('          cd "$RUNNER_TEMP"\n', ""),
                "changed_cd": workflow.replace(
                    '          cd "$RUNNER_TEMP"', '          cd "$root"'
                ),
                "early_exit": workflow.replace(command, "          exit 0\n" + command),
                "disable_failure": workflow.replace(
                    command, "          set +e\n" + command
                ),
                "later_cd": workflow.replace(
                    command, '          cd "$root"\n' + command
                ),
                "conditional_step": workflow.replace(
                    "      - name: " + STEP + "\n",
                    "      - name: " + STEP + "\n        if: false\n",
                ),
                "conditional_job": workflow.replace(
                    "  installed:\n", "  installed:\n    if: false\n"
                ),
                "ignored_job": workflow.replace(
                    "  installed:\n", "  installed:\n    continue-on-error: true\n"
                ),
                "upload_comment": workflow.replace(
                    upload, "            # " + upload.strip()
                ),
                "upload_no_action": workflow.replace(
                    "      - uses: " + UPLOAD + "\n        if: always()",
                    "      - name: upload disabled\n        if: always()",
                ),
                "conditional_upload": workflow.replace(
                    "        if: always()", "        if: success()"
                ),
                "missing_timeout": workflow.replace(timeout, ""),
                "commented_timeout": workflow.replace(
                    timeout, "    # timeout-minutes: 45\n"
                ),
                "duplicate_timeout": workflow.replace(timeout, timeout + timeout),
                "timeout_in_native_job": workflow.replace(timeout, "").replace(
                    "    timeout-minutes: 90\n", timeout
                ),
                "timeout_in_step": workflow.replace(timeout, "").replace(
                    "      - name: " + STEP + "\n",
                    "      - name: " + STEP + "\n    " + timeout,
                ),
                "timeout_in_matrix": workflow.replace(timeout, "").replace(
                    "        python: ['3.11', '3.14.6']\n",
                    "        python: ['3.11', '3.14.6']\n    " + timeout,
                ),
                "timeout_after_steps": workflow.replace(timeout, "") + timeout,
                "timeout_in_global_env": workflow.replace(timeout, "").replace(
                    "env:\n", "env:\n  timeout-minutes: 45\n"
                ),
                "lower_step_timeout": workflow.replace(
                    "      - name: " + STEP + "\n",
                    "      - name: " + STEP + "\n        timeout-minutes: 20\n",
                ),
            }
            # Every lower integer, disabled/indirect spelling, and larger budget
            # must fail closed; only the exact reviewed job allowance is valid.
            for value in (*range(45), -1, 46, 90, "null", "false", "'45'", "${{ 45 }}"):
                mutations["changed_timeout_" + str(value)] = workflow.replace(
                    timeout, "    timeout-minutes: " + str(value) + "\n"
                )
            for control in (
                "if false; then",
                "wheel=wrong",
                "return 0",
                "exec true",
                "trap true EXIT",
            ):
                mutations["extra_control_" + control] = workflow.replace(
                    command, "          " + control + "\n" + command
                )
            mutations["step_metadata_after_run"] = workflow.replace(
                "      - uses: " + UPLOAD + "\n        if: always()",
                "        continue-on-error: true\n      - uses: "
                + UPLOAD
                + "\n        if: always()",
            )
            mutations["quoted_conditional_job"] = workflow.replace(
                "  installed:\n", '  installed:\n    "if": false\n'
            )
            mutations["quoted_ignore_job"] = workflow.replace(
                "  installed:\n", '  installed:\n    "continue-on-error": true\n'
            )
            mutations["global_shell_defaults"] = workflow.replace(
                "jobs:\n", "defaults:\n  run:\n    shell: bash {0}\njobs:\n"
            )
            mutations["job_shell_defaults"] = workflow.replace(
                "  installed:\n",
                "  installed:\n    defaults:\n      run:\n        shell: bash {0}\n",
            )
            mutations["job_metadata_after_steps"] = workflow + '    "if": false\n'
            mutations["negative_upload"] = workflow.replace(
                upload,
                upload + "\n            !${{ runner.temp }}/" + report + "*.json",
            )
            mutations["quoted_negative_upload"] = workflow.replace(
                upload,
                upload + '\n            "!${{ runner.temp }}/' + report + '*.json"',
            )
            mutations["late_global_shell_defaults"] = (
                workflow + "defaults:\n  run:\n    shell: bash {0}\n"
            )
            mutations["late_flow_defaults"] = (
                workflow + "defaults: {run: {shell: 'bash {0}'}}\n"
            )
            mutations["blank_negative_upload"] = workflow.replace(
                upload,
                upload + "\n\n            !${{ runner.temp }}/" + report + "*.json",
            )
            mutations["comment_negative_upload"] = workflow.replace(
                upload,
                upload
                + "\n# comment\n            !${{ runner.temp }}/"
                + report
                + "*.json",
            )
            for name, changed in mutations.items():
                with self.subTest(surface=surface, mutation=name):
                    self.assertNotEqual(changed, workflow)
                    with self.assertRaises(AssertionError):
                        require_installed_gate(changed, script, report, extra)


if __name__ == "__main__":
    unittest.main()
