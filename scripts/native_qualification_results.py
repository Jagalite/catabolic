# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Require every distinct declared fixture to finish successfully, without skips."""

import unittest


class QualificationResult(unittest.TextTestResult):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.successful_scenarios = []

    def addSuccess(self, test):
        super().addSuccess(test)
        self.successful_scenarios.append(test.id())


def scenario_ids(suite):
    for value in suite:
        if isinstance(value, unittest.TestSuite):
            yield from scenario_ids(value)
        else:
            yield value.id()


def run_required_suite(suite, expected_cases, expected_count):
    identifiers = list(scenario_ids(suite))
    expected = [
        case(name).id() for case, methods in expected_cases.items() for name in methods
    ]
    if (
        expected_count < 1
        or len(identifiers) != expected_count
        or len(expected) != expected_count
    ):
        raise RuntimeError(
            f"expected {expected_count} qualification scenarios, discovered {len(identifiers)}"
        )
    if len(set(identifiers)) != len(identifiers) or len(set(expected)) != len(expected):
        raise RuntimeError("duplicate qualification scenario IDs")
    if set(identifiers) != set(expected):
        raise RuntimeError("unexpected or missing qualification scenario IDs")
    result = unittest.TextTestRunner(verbosity=2, resultclass=QualificationResult).run(
        suite
    )
    if not (
        result.wasSuccessful()
        and not result.skipped
        and not result.expectedFailures
        and not result.unexpectedSuccesses
        and result.testsRun == expected_count
        and len(result.successful_scenarios) == expected_count
        and set(result.successful_scenarios) == set(identifiers)
    ):
        raise RuntimeError(
            "qualification requires every scenario to pass without skips"
        )
    return result
