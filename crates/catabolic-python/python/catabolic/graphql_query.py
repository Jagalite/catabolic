# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Opt-in compatibility names; behavior remains owned by catabolic_native.graphql_query."""

from catabolic_native.graphql_query import SDL as SDL
from catabolic_native.graphql_query import execute_graphql as execute_graphql

from catabolic._disposition import getattr_for as _getattr_for

__getattr__ = _getattr_for(__name__)
