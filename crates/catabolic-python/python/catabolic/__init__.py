# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Opt-in compatibility names; behavior remains owned by catabolic_native."""

from catabolic_native import __version__ as __version__

from catabolic._disposition import getattr_for as _getattr_for
from catabolic._disposition import install as _install

__getattr__ = _getattr_for(__name__)
_install()
