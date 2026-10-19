# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Opt-in compatibility names; behavior remains owned by catabolic_native.layouts."""

from catabolic_native.layouts import PRESETS as PRESETS
from catabolic_native.layouts import Layouts as Layouts

from catabolic._disposition import getattr_for as _getattr_for

__getattr__ = _getattr_for(__name__)
