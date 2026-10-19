"""Opt-in, read-only native catalog inspection for trusted operators.

Native owners produce the complete results and diagnostic errors. A plan does
not establish quiescence, authorize upgrade, or qualify a release. This module
does not expose application-user authority or change legacy schema defaults.
"""

import json

from . import _native


def inspect_database(path, *, full=False):
    return json.loads(_native.native_catalog_inspect(str(path), full))


def plan_upgrade(path):
    return json.loads(_native.native_catalog_plan_upgrade(str(path)))
