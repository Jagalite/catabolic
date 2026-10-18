# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Opt-in compatibility names; behavior remains owned by catabolic_native.catalog_refresh."""

from catabolic_native.catalog_refresh import CatalogRefresh as CatalogRefresh
from catabolic_native.catalog_refresh import enqueue as enqueue
from catabolic_native.catalog_refresh import finish as finish

from catabolic._disposition import getattr_for as _getattr_for

__getattr__ = _getattr_for(__name__)
