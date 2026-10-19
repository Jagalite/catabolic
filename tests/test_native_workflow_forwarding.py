# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Isolated persistent-facade forwarding tests; no native business logic runs."""

import importlib.util
import unittest
from pathlib import Path
from types import SimpleNamespace

FACADE = (
    Path(__file__).resolve().parents[1]
    / "crates/catabolic-python/python/catabolic_native"
)


def facade(name):
    spec = importlib.util.spec_from_file_location(name, FACADE / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class RecordingStore:
    def __init__(self):
        self.calls = []
        self.result = object()

    def _call(self, *args):
        self.calls.append(args)
        return self.result


class WorkflowForwarding(unittest.TestCase):
    def setUp(self):
        self.store = RecordingStore()
        self.app = SimpleNamespace(store=self.store, profile="alternate")
        self.workflow = facade("item_workflow").ItemWorkflow(self.app)
        self.curation_module = facade("curation")
        self.curation = self.curation_module.Curation(self.app)

    def assert_call(self, service, method, args, kwargs, operation, request):
        self.store.calls.clear()
        returned = getattr(service, method)(*args, **kwargs)
        self.assertIs(returned, self.store.result)
        surface = "item_workflow" if service is self.workflow else "curation"
        self.assertEqual(self.store.calls, [(surface, operation, request, "alternate")])

    def test_all_workflow_defaults_are_explicit_and_returned_unchanged(self):
        cases = (
            ("get", ("item",), {}, "get", {"item_id": "item"}),
            (
                "checks",
                ("item",),
                {},
                "checks",
                {"item_id": "item", "limit": 100, "after": ""},
            ),
            (
                "note",
                ("item", "body"),
                {},
                "note",
                {
                    "item_id": "item",
                    "body": "body",
                    "kind": "note",
                    "actor": "user",
                    "expected_revision": None,
                },
            ),
            (
                "set_status",
                ("item", "pending"),
                {},
                "status",
                {
                    "item_id": "item",
                    "status": "pending",
                    "note": "",
                    "actor": "user",
                    "expected_revision": None,
                },
            ),
            (
                "log",
                ("item",),
                {},
                "log",
                {"item_id": "item", "limit": 100, "after": 0},
            ),
            (
                "require",
                ("item", "review", "label"),
                {},
                "require",
                {
                    "item_id": "item",
                    "kind": "review",
                    "label": "label",
                    "target": None,
                    "actor": "user",
                    "note": "",
                    "expected_revision": None,
                },
            ),
            (
                "resolve",
                ("requirement", "complete"),
                {"note": "done"},
                "resolve",
                {
                    "requirement_id": "requirement",
                    "state": "complete",
                    "note": "done",
                    "actor": "user",
                    "expected_revision": None,
                },
            ),
        )
        for method, args, kwargs, operation, request in cases:
            with self.subTest(method=method):
                self.assert_call(
                    self.workflow, method, args, kwargs, operation, request
                )

    def test_all_curation_defaults_are_explicit_and_returned_unchanged(self):
        payload = {"item_id": "item"}
        cases = (
            (
                "snapshot",
                ("file", payload),
                {},
                "snapshot",
                {"file_id": "file", "payload": payload},
            ),
            (
                "put",
                ("file", payload),
                {},
                "proposal-put",
                {
                    "file_id": "file",
                    "payload": payload,
                    "source": "manual",
                    "evidence": None,
                },
            ),
            ("get", ("proposal",), {}, "proposal-get", {"id": "proposal"}),
            (
                "accept_existing_association_evidence",
                ("proposal",),
                {"actor": "reviewer"},
                "proposal-accept-existing",
                {"id": "proposal", "actor": "reviewer"},
            ),
            (
                "list",
                (),
                {},
                "proposal-list",
                {"state": None, "limit": 100, "after": ""},
            ),
            (
                "decide",
                ("proposal",),
                {"accept": False, "actor": "reviewer"},
                "proposal-decide",
                {"id": "proposal", "accept": False, "actor": "reviewer"},
            ),
            ("history", (), {}, "proposal-history", {"limit": 100, "after": 0}),
            (
                "sidecars",
                (),
                {},
                "sidecars",
                {"location": None, "limit": 100, "after": "", "apply": False},
            ),
            (
                "expected_put",
                ("collection", payload),
                {},
                "expected-put",
                {"collection": "collection", "value": payload},
            ),
            (
                "completeness",
                ("expected",),
                {},
                "completeness",
                {"id": "expected", "limit": 100, "after": 0},
            ),
        )
        for method, args, kwargs, operation, request in cases:
            with self.subTest(method=method):
                self.assert_call(
                    self.curation, method, args, kwargs, operation, request
                )

    def test_optional_arguments_are_forwarded_without_truthiness_coercion(self):
        cases = (
            (
                self.workflow,
                "checks",
                ("item",),
                {"limit": 2, "after": "cursor"},
                "checks",
                {"item_id": "item"},
            ),
            (
                self.workflow,
                "note",
                ("item", "body"),
                {"kind": "progress", "actor": "reviewer", "expected_revision": 0},
                "note",
                {"item_id": "item", "body": "body"},
            ),
            (
                self.workflow,
                "set_status",
                ("item", "in_progress"),
                {"note": "review", "actor": "reviewer", "expected_revision": 0},
                "status",
                {"item_id": "item", "status": "in_progress"},
            ),
            (
                self.workflow,
                "log",
                ("item",),
                {"limit": 2, "after": 3},
                "log",
                {"item_id": "item"},
            ),
            (
                self.workflow,
                "require",
                ("item", "file", "label"),
                {
                    "target": "file",
                    "actor": "reviewer",
                    "note": "review",
                    "expected_revision": 0,
                },
                "require",
                {"item_id": "item", "kind": "file", "label": "label"},
            ),
            (
                self.workflow,
                "resolve",
                ("requirement", "waived"),
                {"note": "review", "actor": "reviewer", "expected_revision": 0},
                "resolve",
                {"requirement_id": "requirement", "state": "waived"},
            ),
            (
                self.curation,
                "put",
                ("file", {"item_id": "item"}),
                {"source": "fixture", "evidence": {}},
                "proposal-put",
                {"file_id": "file", "payload": {"item_id": "item"}},
            ),
            (
                self.curation,
                "list",
                (),
                {"state": "pending", "limit": 2, "after": True},
                "proposal-list",
                {},
            ),
            (
                self.curation,
                "history",
                (),
                {"limit": 2, "after": "3"},
                "proposal-history",
                {},
            ),
            (
                self.curation,
                "sidecars",
                (),
                {"location": "media", "limit": 2, "after": 1.5, "apply": True},
                "sidecars",
                {},
            ),
            (
                self.curation,
                "completeness",
                ("expected",),
                {"limit": 2, "after": 3},
                "completeness",
                {"id": "expected"},
            ),
        )
        for service, method, args, kwargs, operation, request in cases:
            with self.subTest(method=method):
                self.assert_call(
                    service, method, args, kwargs, operation, request | kwargs
                )

    def test_constructors_retain_original_app_store_and_profile(self):
        self.app.store = RecordingStore()
        self.app.profile = "changed"
        for service in (self.workflow, self.curation):
            self.assertIs(service.app, self.app)
            self.assertIs(service.store, self.store)
            self.assertEqual(service.profile, "alternate")
        self.assert_call(
            self.workflow, "get", ("item",), {}, "get", {"item_id": "item"}
        )
        self.assert_call(
            self.curation, "get", ("proposal",), {}, "proposal-get", {"id": "proposal"}
        )

    def test_occurrence_uses_supplied_store_and_profile(self):
        returned = self.curation_module.occurrence(self.store, "other", "file")
        self.assertIs(returned, self.store.result)
        self.assertEqual(
            self.store.calls,
            [("curation", "occurrence", {"file_id": "file"}, "other")],
        )

    def test_owner_errors_are_not_retried_or_replaced(self):
        marker = RuntimeError("original owner error")

        def fail(*args):
            self.store.calls.append(args)
            raise marker

        self.store._call = fail
        for action in (
            lambda: self.workflow.note("item", "body"),
            lambda: self.curation.put("file", {"item_id": "item"}),
            lambda: self.curation_module.occurrence(self.store, "other", "file"),
        ):
            self.store.calls.clear()
            with self.assertRaises(RuntimeError) as caught:
                action()
            self.assertIs(caught.exception, marker)
            self.assertEqual(len(self.store.calls), 1)


if __name__ == "__main__":
    unittest.main()
