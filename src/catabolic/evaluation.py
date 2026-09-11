# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Supported, caller-bound evaluation budgets for existing query engines."""

import time

from .domain import CatabolicError
from .store import encode


class EvaluationSession:
    def __init__(
        self,
        store,
        profile="default",
        *,
        access=None,
        http=False,
        timeout_ms=5000,
        max_ids=10000,
        max_bytes=4 * 1024 * 1024,
        evaluation_time=None,
        observations=(),
    ):
        self.store, self.profile, self.access = store, profile, access
        self.database_id = store.database_id
        self.epoch = store.rows("SELECT generation FROM fallback_epoch WHERE id=1")[0][
            "generation"
        ]
        self.evaluation_time = (
            time.time() if evaluation_time is None else evaluation_time
        )
        self.observations = tuple(observations)
        self.cancelled = False
        self.max_bytes, self.result_bytes = max_bytes, 0
        self.context = dict(
            deadline=time.monotonic() + timeout_ms / 1000,
            nodes=0,
            count=0,
            maximum=max_ids,
            cache={},
            http=http or access is not None,
            access=access,
        )

    def check(self, store, profile):
        if (
            store is not self.store
            or profile != self.profile
            or store.database_id != self.database_id
        ):
            raise CatabolicError("evaluation_session_identity_mismatch")
        if (
            store.rows("SELECT generation FROM fallback_epoch WHERE id=1")[0][
                "generation"
            ]
            != self.epoch
        ):
            raise CatabolicError("stale_evaluation_snapshot")
        if self.cancelled:
            raise CatabolicError("evaluation_cancelled")
        if time.monotonic() >= self.context["deadline"]:
            raise CatabolicError("query composition timed out")

    def select(self, identifier):
        from .saved_queries import Queries

        return Queries(self.store, self.profile).select(identifier, session=self)

    def account(self, result):
        size = len(encode(result).encode())
        if self.result_bytes + size > self.max_bytes:
            raise CatabolicError("evaluation_result_byte_budget")
        self.result_bytes += size

    def pause_for_io(self, seconds):
        # Query execution time excludes separately bounded live filesystem probes.
        self.context["deadline"] += seconds
