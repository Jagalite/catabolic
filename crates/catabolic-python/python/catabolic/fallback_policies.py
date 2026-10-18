# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Opt-in compatibility names; behavior remains owned by catabolic_native.fallback_policies."""

from catabolic_native.fallback_policies import Policies as Policies

from catabolic._disposition import getattr_for as _getattr_for

__getattr__ = _getattr_for(__name__)
