# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Opt-in compatibility names; behavior remains owned by catabolic_native.http.contract."""

from catabolic_native.http.contract import VERSION as VERSION

from catabolic._disposition import getattr_for as _getattr_for

__getattr__ = _getattr_for(__name__)
