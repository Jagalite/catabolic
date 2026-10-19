"""Native same-owner saved-query sessions with cumulative budgets and caches.

This adapter supports local saved-query evaluation, not foreign Python access
contexts or mutable Python resolver/cache dictionaries. Its additional methods
are not part of this checkpoint's ten qualified inventory rows.
"""

import time

from .store import CatabolicError, Store


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
        if not isinstance(store, Store):
            raise CatabolicError("evaluation requires a catabolic_native.store.Store")
        if access is not None:
            raise CatabolicError(
                "native evaluation access contexts require the native scoped-query owner; "
                "Python Access objects are unsupported by this facade"
            )
        self.store, self.profile, self.access = store, profile, access
        self.database_id = store.database_id
        self.cancelled = False
        self.evaluation_time = (
            time.time() if evaluation_time is None else evaluation_time
        )
        self.observations = tuple(observations)
        result = store._call(
            "query_execution",
            "evaluation-create",
            dict(
                http=bool(http),
                timeout_ms=timeout_ms,
                max_ids=max_ids,
                max_bytes=max_bytes,
            ),
            profile,
        )
        self._token, self.epoch = result["token"], result["epoch"]

    def _call(self, operation, **request):
        return self.store._call(
            "query_execution",
            "evaluation-" + operation,
            dict(session=self._token, cancelled=self.cancelled, **request),
            self.profile,
        )

    def check(self, store, profile):
        if store is not self.store or profile != self.profile:
            raise CatabolicError("evaluation_session_identity_mismatch")
        self._call("check")

    def select(self, identifier):
        from .saved_queries import Queries

        return Queries(self.store, self.profile).select(identifier, session=self)

    def account(self, result):
        self._call("account", value=result)

    def pause_for_io(self, seconds):
        self._call("pause", seconds=seconds)

    def rows(self, sql, params=(), *, maximum=None):
        raise CatabolicError(
            "native EvaluationSession.rows is not implemented; use saved SQL queries "
            "for bounded evaluation on this Store"
        )

    def close(self):
        self._call("drop")

    def __del__(self):
        if hasattr(self, "_token"):
            try:
                self.close()
            except Exception:
                pass
