# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Opt-in compatibility names; behavior remains owned by catabolic_native.component_sql."""

from catabolic_native.component_sql import OCCURRENCES_SQL as OCCURRENCES_SQL

from catabolic._disposition import getattr_for as _getattr_for

__getattr__ = _getattr_for(__name__)
