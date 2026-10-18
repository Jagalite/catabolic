# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Opt-in compatibility names; behavior remains owned by catabolic_native.app."""

from catabolic_native.app import Application as Application
from catabolic_native.app import BoundRoot as BoundRoot

from catabolic._disposition import getattr_for as _getattr_for

__getattr__ = _getattr_for(__name__)
