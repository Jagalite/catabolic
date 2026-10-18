# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Opt-in HTTP names; native production admission remains gated."""

from catabolic._disposition import getattr_for as _getattr_for

__getattr__ = _getattr_for(__name__)
