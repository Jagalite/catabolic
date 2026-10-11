"""Immutable saved-query contracts on the caller's persistent native Store."""

from .store import CatabolicError


class Queries:
    def __init__(self, store, profile="default"):
        self.store, self.profile = store, profile

    def _call(self, operation, **request):
        return self.store._call(
            "saved_queries", "query-" + operation, request, self.profile
        )

    def get(self, identifier):
        return self._call("get", id=identifier)

    def list(self, limit=100, after=""):
        return self._call("list", limit=limit, after=after)

    def validate(self, value):
        return self._call("validate", value=value)

    def put(self, query_name, value):
        return self._call("put", name=query_name, value=value)

    def _execute(self, operation, identifier, session, **request):
        if session is not None:
            from .evaluation import EvaluationSession

            if not isinstance(session, EvaluationSession):
                raise CatabolicError(
                    "session requires a same-owner catabolic_native.evaluation.EvaluationSession; "
                    "foreign Python evaluation/access contexts are unsupported"
                )
            # Native dispatch validates the run limit and looks up the query
            # before checking session identity, matching the frozen contract.
            # A foreign owner's token is never consumed by this Store.
            request.update(
                session=session._token,
                cancelled=session.cancelled,
                session_owner_matches=session.store is self.store,
            )
        return self.store._call(
            "query_execution", operation, {"id": identifier, **request}, self.profile
        )

    def select(self, identifier, *, _http=False, session=None):
        entity, ids, report = self._execute(
            "select", identifier, session, _http=bool(_http)
        )
        return entity, set(ids), report

    def run(self, identifier, limit=1000, *, session=None):
        return self._execute("run", identifier, session, limit=limit)
