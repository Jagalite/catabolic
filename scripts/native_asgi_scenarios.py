# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Exact installed ASGI inventory and pre-import aggregate routing.

Deferring these files never satisfies their installed-wheel prerequisite. This
manifest is shared by the aggregate and the separate installed-artifact gate.
"""

import ast
import hashlib
import sys
import unittest
from pathlib import Path

REQUIRED_CASES = {
    "tests.test_rust_asgi_adapter.ASGIAdapterTest": (
        "test_exact_signature_and_ordinary_constructor_refusal",
        "test_native_and_python_refuse_remote_missing_and_proxy_peers_before_effects",
        "test_fresh_capability_and_live_owner_cleanup",
        "test_fixture_catalog_identity_cannot_be_replaced",
        "test_lifespan_callback_failure_preserves_exception_and_closes",
        "test_paired_identity_methods_and_content_frames",
        "test_paired_hosts_cors_and_body_bounds",
        "test_persistent_rate_state_and_application_isolation",
        "test_chunked_request_and_response_streaming",
        "test_disconnect_and_cancel_release_streams_for_reuse",
        "test_callback_exception_identity_and_cleanup",
        "test_delayed_bodies_progress_with_one_executor_worker",
        "test_delayed_body_cancellation_and_close_do_not_need_executor_slots",
        "test_shutdown_stops_new_calls_before_background_close_runs",
        "test_request_body_cancellation_and_lifespan_shutdown",
    ),
    "tests.test_rust_asgi_smoke.InstalledASGISmoke": (
        "test_real_http_application_without_framework",
        "test_ordinary_paths_and_remote_peers_are_refused",
        "test_streamed_request_body_is_bounded",
        "test_cancelled_partial_body_retires_native_request",
        "test_delayed_body_concurrency_with_one_executor_thread",
        "test_lifespan_is_terminal_and_cleanup_is_owned",
    ),
}
REQUIRED_IDS = tuple(
    f"{case}.{method}" for case, methods in REQUIRED_CASES.items() for method in methods
)
SMOKE_IDS = tuple(
    name for name in REQUIRED_IDS if name.startswith("tests.test_rust_asgi_smoke.")
)
REQUIRED_FILES = (
    "tests/test_rust_asgi_adapter.py",
    "tests/test_rust_asgi_smoke.py",
)


def _docstring(node):
    return (
        isinstance(node, ast.Expr)
        and isinstance(node.value, ast.Constant)
        and isinstance(node.value.value, str)
    )


def _validate_module_routing(tree, relative):
    # Top-level assignments/calls or class-body aliases can add, remove or
    # multiply unittest IDs without declaring another test method.
    main_guard = ast.dump(
        ast.parse('if __name__ == "__main__":\n    unittest.main()').body[0]
    )
    for node in tree.body:
        if isinstance(node, (ast.Import, ast.ImportFrom, ast.ClassDef)) or _docstring(
            node
        ):
            continue
        if (
            isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            and not node.decorator_list
        ):
            continue
        if isinstance(node, ast.If) and ast.dump(node) == main_guard:
            continue
        raise ValueError(f"unexpected installed ASGI module routing: {relative}")


def validate_sources(root):
    """Verify every deferred file and declared scenario without importing it.

    The prefix scan detects unexpected files; only the two exact paths below
    can ever be deferred. Duplicate declarations cannot collapse into a set.
    """
    root = Path(root).resolve(strict=True)
    if (root / "tests").is_symlink() or not (root / "tests").is_dir():
        raise ValueError("missing or linked installed ASGI tests directory")
    actual_files = sorted(
        str(path.relative_to(root))
        for path in (root / "tests").rglob("test_rust_asgi*.py")
    )
    if actual_files != sorted(REQUIRED_FILES):
        raise ValueError(f"installed ASGI file inventory drift: {actual_files!r}")
    if len(REQUIRED_IDS) != len(set(REQUIRED_IDS)):
        raise ValueError("duplicate installed ASGI manifest scenario IDs")
    declared = []
    hashes = {}
    for relative in REQUIRED_FILES:
        path = root / relative
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"missing or linked installed ASGI source: {relative}")
        content = path.read_bytes()
        tree = ast.parse(content, filename=str(path))
        _validate_module_routing(tree, relative)
        module = relative.removesuffix(".py").replace("/", ".")
        expected_classes = {
            name.rsplit(".", 1)[1]
            for name in REQUIRED_CASES
            if name.rsplit(".", 1)[0] == module
        }
        classes = [node for node in tree.body if isinstance(node, ast.ClassDef)]
        found_classes = [node.name for node in classes if node.name in expected_classes]
        if sorted(found_classes) != sorted(expected_classes):
            raise ValueError(f"installed ASGI class inventory drift: {relative}")
        accepted_methods = set()
        for node in classes:
            if node.name not in expected_classes:
                # An inherited test class with no own test methods would still
                # multiply the installed suite. Only support exceptions exist.
                if (
                    [ast.unparse(base) for base in node.bases] != ["BaseException"]
                    or node.decorator_list
                    or node.keywords
                ):
                    raise ValueError(f"unexpected installed ASGI class: {node.name}")
                continue
            if (
                [ast.unparse(base) for base in node.bases]
                != ["unittest.IsolatedAsyncioTestCase"]
                or node.decorator_list
                or node.keywords
            ):
                raise ValueError(
                    f"unexpected installed ASGI class routing: {node.name}"
                )
            for item in node.body:
                if _docstring(item) or isinstance(item, ast.Pass):
                    continue
                if (
                    not isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef))
                    or item.decorator_list
                ):
                    raise ValueError(
                        f"unexpected installed ASGI class body routing: {node.name}"
                    )
            methods = [
                item
                for item in node.body
                if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef))
                and item.name.startswith("test_")
            ]
            name = f"{module}.{node.name}"
            identifiers = [f"{name}.{method.name}" for method in methods]
            expected = [f"{name}.{method}" for method in REQUIRED_CASES[name]]
            if sorted(identifiers) != sorted(expected):
                raise ValueError(f"installed ASGI scenario inventory drift: {name}")
            for method in methods:
                if method.decorator_list:
                    raise ValueError(
                        f"decorated installed ASGI scenario: {method.name}"
                    )
                accepted_methods.add(id(method))
            declared.extend(identifiers)
        for node in ast.walk(tree):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                if node.name == "load_tests" or (
                    node.name.startswith("test_") and id(node) not in accepted_methods
                ):
                    raise ValueError(
                        f"unexpected installed ASGI scenario routing: {relative}"
                    )
        hashes[relative] = hashlib.sha256(content).hexdigest()
    if sorted(declared) != sorted(REQUIRED_IDS):
        raise ValueError("missing or duplicate installed ASGI scenario IDs")
    return hashes


def installed_prerequisite(source_sha256):
    return {
        "name": "installed_asgi",
        "mandatory": True,
        "status": "pending",
        "manifest_verified": True,
        "gate": "scripts/native_asgi_installed_tests.py",
        "workflow": ".github/workflows/native-qualification.yml",
        "workflow_step": "Mandatory installed disposable ASGI fixtures, 21 full or 6 minimal",
        "reason": "Requires the installed release wheel; an explicit debug extension is insufficient",
        "source_sha256": source_sha256,
        "required_scenarios": list(REQUIRED_IDS),
        "required_lanes": [
            {
                "python": "3.14.6",
                "gate_arguments": ["--lane", "full"],
                "scenario_ids": list(REQUIRED_IDS),
            },
            {
                "python": "3.11",
                "gate_arguments": ["--lane", "minimal"],
                "scenario_ids": list(SMOKE_IDS),
            },
        ],
    }


class InstalledASGIDeferredLoader(unittest.TestLoader):
    """Exclude only an exact, verified file before unittest imports it."""

    def __init__(self, root):
        super().__init__()
        self.root = Path(root).resolve(strict=True)
        self.source_sha256 = validate_sources(self.root)
        self.deferred_files = []
        self.expected_paths = {self.root / name: name for name in REQUIRED_FILES}
        modules = {name.rsplit(".", 1)[0] for name in REQUIRED_CASES}
        if any(name in sys.modules for name in modules):
            raise ValueError(
                "installed ASGI prerequisites were imported before deferral"
            )

    def _match_path(self, path, full_path, pattern):
        absolute = Path(full_path).absolute()
        if absolute in self.expected_paths:
            if not super()._match_path(path, full_path, pattern):
                raise ValueError(
                    "discovery pattern does not include all installed ASGI prerequisites"
                )
            self.deferred_files.append(self.expected_paths[absolute])
            return False
        if Path(path).name.startswith("test_rust_asgi"):
            raise ValueError(f"unexpected installed ASGI discovery path: {full_path}")
        return super()._match_path(path, full_path, pattern)

    def verify_routing(self, identifiers):
        if sorted(self.deferred_files) != sorted(REQUIRED_FILES):
            raise ValueError("missing or duplicate installed ASGI discovery routing")
        if any(name.startswith("tests.test_rust_asgi") for name in identifiers):
            raise ValueError(
                "installed ASGI prerequisite leaked into selected aggregate"
            )
        modules = {name.rsplit(".", 1)[0] for name in REQUIRED_CASES}
        if any(name in sys.modules for name in modules):
            raise ValueError(
                "installed ASGI prerequisite imported during aggregate discovery"
            )
        if validate_sources(self.root) != self.source_sha256:
            raise ValueError("installed ASGI source changed during aggregate discovery")
        return installed_prerequisite(self.source_sha256)
