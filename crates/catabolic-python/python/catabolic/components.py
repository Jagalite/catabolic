# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Opt-in compatibility names; behavior remains owned by catabolic_native.components."""

from catabolic_native.components import backfill as backfill
from catabolic_native.components import get as get
from catabolic_native.components import index_file as index_file
from catabolic_native.components import revision as revision

from catabolic._disposition import getattr_for as _getattr_for

__getattr__ = _getattr_for(__name__)
