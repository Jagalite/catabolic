# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Explicit disposable-fixture admission; never accepts an existing catalog.

The returned native capability owns a fresh catalog. Seed only disposable data
at its read-only ``path`` property. Close all ASGI apps (lifespan shutdown or
``await app.aclose()``), then call ``fixture.close()`` to remove the temporary
catalog. Dropping the capability also cleans up after all native owners retire.
Do not put credentials, production catalogs, or real media into this fixture.
"""

from .. import _native


def disposable_fixture():
    """Create a new temporary catalog and its non-path admission capability."""
    return _native.DisposableHTTPFixture()
