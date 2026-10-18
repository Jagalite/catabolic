# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Opt-in compatibility names; behavior remains owned by catabolic_native.access."""

from catabolic_native.access import AccessError as AccessError
from catabolic_native.access import grant_put as grant_put
from catabolic_native.access import issue as issue
from catabolic_native.access import principal_put as principal_put

from catabolic._disposition import getattr_for as _getattr_for

__getattr__ = _getattr_for(__name__)
# The 0.2 module was a package; its removed submodules answer with the
# API disposition instead of a bare 'not a package' error.
__path__: list[str] = []
