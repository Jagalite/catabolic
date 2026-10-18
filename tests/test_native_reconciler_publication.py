# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native ``Reconciler.apply``/``recover`` against the frozen reference (track C3).

The frozen Python reference owns fixture setup and every assertion. Each
native call opens the persistent ``catabolic_native`` Store on the same
catalog while the reference Store is detached, so the engines never hold the
writer at the same time.

Requires ``CATABOLIC_NATIVE_EXTENSION_ROOT``: a directory that contains an
importable ``catabolic_native`` package with its built ``_native`` extension.
"""

import json
import os
import sys
import unittest

from catabolic.reconcile import Reconciler
from tests import test_application as reference

EXTENSION = os.environ.get("CATABOLIC_NATIVE_EXTENSION_ROOT")
if EXTENSION:
    sys.path.insert(0, EXTENSION)
    import catabolic_native


class NativeFacadeReconciler:
    """The reference test's reconciler, run by the native public facade."""

    def __init__(self, owner, notify_consumers=True):
        self.owner = owner
        self.notify_consumers = notify_consumers

    def call(self, method, *args, **kwargs):
        with self.owner.store.detached():
            with catabolic_native.Store(self.owner.database, writable=True) as store:
                reconciler = catabolic_native.Reconciler(
                    catabolic_native.Application(store),
                    notify_consumers=self.notify_consumers,
                )
                return getattr(reconciler, method)(*args, **kwargs)

    def preview(self, *args, **kwargs):
        return self.call("preview", *args, **kwargs)

    def apply(self, *args, **kwargs):
        return self.call("apply", *args, **kwargs)

    def recover(self, *args, **kwargs):
        return self.call("recover", *args, **kwargs)

    def verify(self, *args, **kwargs):
        return self.call("verify", *args, **kwargs)


@unittest.skipUnless(
    EXTENSION, "set CATABOLIC_NATIVE_EXTENSION_ROOT to the built extension"
)
class NativeFacadeReferenceTest(unittest.TestCase):
    def setUp(self):
        reference.CatalogTest.setUp(self)
        self.reconciler = NativeFacadeReconciler(self)


# Unchanged reference assertions whose only engine calls are reconciler calls.
# confirmed_missing_removes_link_and_restores_mapping is excluded: it removes a
# symlink, which the native facade refuses while the replace/remove gate is
# closed (see test_removal_and_forget_match_frozen).
REFERENCE = (
    "end_to_end_and_repeat_preserves_source_and_link",
    "preview_is_read_only",
    "missing_without_scan_blocks_all_changes",
    "changed_root_blocks_scan_and_sync",
    "missing_root_blocks_scan_and_sync",
    "source_symlink_is_not_followed",
    "source_changed_requires_rescan",
    "empty_source_is_blocked",
    "unknown_output_is_never_claimed",
    "all_catalogs_preflight_blocks_before_any_mutation",
    "wrong_ownership_blocks",
    "symlink_parent_blocks_without_touching_external_tree",
    "external_regular_file_is_not_replaced_or_removed",
    "external_retargeted_symlink_is_not_removed",
    "incomplete_download_suffix_is_never_projected",
)
for name in REFERENCE:
    setattr(
        NativeFacadeReferenceTest,
        "test_" + name,
        getattr(reference.CatalogTest, "test_" + name),
    )


# Per-fixture identities: twin catalogs differ in roots, inode numbers,
# database identity, run identifiers and clocks, never in decisions.
VOLATILE = {"id", "database_id", "device", "inode", "volume_uuid", "elapsed_seconds"}


def normalized(value, root):
    """Drop per-fixture identities and absolute roots from engine output."""
    text = json.dumps(value, sort_keys=True).replace(str(root), "<root>")

    def walk(node):
        if isinstance(node, dict):
            result = {}
            for key, item in node.items():
                if key in VOLATILE or key.endswith("_at"):
                    continue
                if isinstance(item, str) and item[:1] in "{[":
                    try:
                        item = json.loads(item)
                    except ValueError:
                        pass
                result[key] = walk(item)
            return result
        if isinstance(node, list):
            return [walk(item) for item in node]
        return node

    return walk(json.loads(text))


TABLES = (
    "SELECT profile,catalog,path,kind,target,previous FROM journal ORDER BY rowid",
    "SELECT * FROM owned_links ORDER BY path",
    "SELECT profile,catalog,local_binding,generation,verified FROM publication_generations",
    "SELECT * FROM refresh_dirty",
    "SELECT key,value FROM meta WHERE key LIKE 'statistics:%' ORDER BY key",
)


@unittest.skipUnless(
    EXTENSION, "set CATABOLIC_NATIVE_EXTENSION_ROOT to the built extension"
)
class NativeFacadeDifferentialTest(unittest.TestCase):
    """Twin catalogs: frozen apply/recover on one, the native facade on the other."""

    def setUp(self):
        self.frozen = {}
        self.native = {}
        for side in (self.frozen, self.native):
            holder = type("Holder", (), {"addCleanup": staticmethod(self.addCleanup)})()
            reference.CatalogTest.setUp(holder)
            side["holder"] = holder
        self.frozen["engine"] = Reconciler(self.frozen["holder"].app)
        self.native["engine"] = NativeFacadeReconciler(self.native["holder"])

    def state(self, side):
        holder = side["holder"]
        rows = {
            query: normalized(holder.store.rows(query), holder.root) for query in TABLES
        }
        tree = sorted(
            str(path.relative_to(holder.output)) for path in holder.output.rglob("*")
        )
        return rows, tree

    def both(self, method, *args, **kwargs):
        results = {}
        for name, side in (("frozen", self.frozen), ("native", self.native)):
            observed = []

            def record(operation, observed=observed, side=side):
                keys = ("kind", "catalog", "path", "target", "previous", "profile")
                entry = {k: operation[k] for k in keys}
                # Called after the effect: a created link is already visible.
                entry["leaf_present"] = (
                    os.path.lexists(side["holder"].output / operation["path"])
                    if operation["path"]
                    else None
                )
                observed.append(entry)

            call = getattr(side["engine"], method)
            try:
                value = call(*args, after_filesystem=record, **kwargs)
                outcome = ("ok", normalized(value, side["holder"].root))
            except Exception as error:  # noqa: BLE001 - compared across engines
                outcome = ("error", type(error).__name__, str(error))
            results[name] = (outcome, observed, self.state(side))
        return results

    def assertSame(self, results):
        frozen, native = results["frozen"], results["native"]
        self.assertEqual(native[1], frozen[1], "after_filesystem operations")
        self.assertEqual(native[2], frozen[2], "catalog tables and output tree")
        if frozen[0][0] == "error":
            self.assertEqual(native[0][0], "error", native[0])
            self.assertEqual(native[0][2], frozen[0][2])
        else:
            self.assertEqual(native[0], frozen[0])

    def test_apply_then_repeat_matches_frozen(self):
        self.assertSame(self.both("apply"))
        self.assertSame(self.both("apply"))

    def test_recover_with_nothing_pending_matches_frozen(self):
        self.assertSame(self.both("recover"))
        self.assertSame(self.both("recover", None))

    def test_budget_and_catalog_errors_match_frozen(self):
        for kwargs in (
            {"max_removals": -1},
            {"max_removals": True},
            {"max_removal_percent": float("nan")},
            {"max_removal_percent": 101},
        ):
            with self.subTest(kwargs=kwargs):
                self.assertSame(self.both("apply", **kwargs))
        self.assertSame(self.both("apply", "missing"))
        self.assertSame(self.both("recover", "missing"))

    def test_max_changes_matches_frozen(self):
        for value in (0, 1.0, True, -1, 5):
            with self.subTest(max_changes=value):
                self.assertSame(self.both("apply", max_changes=value))

    def test_callback_failure_then_cross_engine_recovery(self):
        # A callback that raises after the effect leaves the same journal in
        # both engines. Each engine then recovers the other engine's journal.
        for kind in ("prepare", "create"):
            with self.subTest(kind=kind):

                def interrupt(operation, kind=kind):
                    if operation["kind"] == kind:
                        raise RuntimeError("simulated process loss")

                for side in (self.frozen, self.native):
                    with self.assertRaisesRegex(RuntimeError, "simulated process loss"):
                        side["engine"].apply(after_filesystem=interrupt)
                self.assertEqual(self.state(self.frozen), self.state(self.native))
                native_recovers_frozen = normalized(
                    NativeFacadeReconciler(self.frozen["holder"]).recover(),
                    self.frozen["holder"].root,
                )
                frozen_recovers_native = normalized(
                    Reconciler(self.native["holder"].app).recover(),
                    self.native["holder"].root,
                )
                # Journal identities differ per engine; compare their counts.
                for result in (native_recovers_frozen, frozen_recovers_native):
                    for key in ("recovered", "cancelled"):
                        result[key] = len(result[key])
                self.assertEqual(native_recovers_frozen, frozen_recovers_native)
                self.assertEqual(native_recovers_frozen["recovered"], 1)
                self.assertEqual(self.state(self.frozen), self.state(self.native))
        self.assertSame(self.both("apply"))

    def test_removal_and_forget_match_frozen(self):
        self.assertSame(self.both("apply"))
        for side in (self.frozen, self.native):
            holder = side["holder"]
            holder.app.disable_mapping(holder.mapping["id"])
        # Native symlink removal stays behind the release/debug gate unless a
        # disposable fixture opts in, so frozen and native plans differ here.
        frozen_plan = self.frozen["engine"].preview()
        native_plan = self.native["engine"].preview()
        self.assertEqual([a["kind"] for a in frozen_plan["actions"]], ["remove"])
        self.assertEqual([a["kind"] for a in native_plan["actions"]], ["remove"])
        self.assertFalse(native_plan["safe"])
        self.assertIn(
            "symlink replacement/removal is blocked",
            json.dumps(native_plan["blockers"]),
        )
        for side in (self.frozen, self.native):
            side["holder"].link.unlink()
        self.assertSame(self.both("apply"))

    def test_notify_consumers_false_matches_frozen(self):
        self.frozen["engine"] = Reconciler(
            self.frozen["holder"].app, notify_consumers=False
        )
        self.native["engine"] = NativeFacadeReconciler(
            self.native["holder"], notify_consumers=False
        )
        self.assertSame(self.both("apply"))
