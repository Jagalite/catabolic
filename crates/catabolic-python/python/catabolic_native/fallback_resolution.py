"""Ordinary fallback resolution on the caller's persistent native Store."""

from .store import CatabolicError


def _session_request(store, profile, session):
    if session is None:
        return {}, None
    from .evaluation import EvaluationSession

    if not isinstance(session, EvaluationSession):
        raise CatabolicError(
            "session requires a same-owner native EvaluationSession; "
            "foreign Python evaluation/access contexts are unsupported"
        )
    return dict(
        session=session._token,
        session_owner_matches=session.store is store and session.profile == profile,
    ), lambda: session.cancelled


class Resolver:
    def __init__(self, app, access=None, operation_id=None, session=None):
        self.app, self.store, self.profile = app, app.store, app.profile
        if access is not None:
            raise CatabolicError(
                "native fallback Access contexts require a native scoped owner; "
                "foreign Python Access objects are unsupported"
            )
        self.access, self.operation_id, self.session = access, operation_id, session
        self.primary_choices, self.total_checks = {}, 0

    def resolve(
        self, entries, policy_id, *, catalog="", previous=None, manual_failback=False
    ):
        request = dict(
            entries=entries,
            policy_id=policy_id,
            catalog=catalog,
            previous=previous,
            manual_failback=bool(manual_failback),
            operation_id=self.operation_id,
            primary_choices=self.primary_choices,
            total_checks=self.total_checks,
        )
        if self.session is None:
            from .evaluation import EvaluationSession

            limits = self.store._call(
                "fallback", "resolver-options", request, self.profile
            )
            self.session = EvaluationSession(
                self.store,
                self.profile,
                timeout_ms=limits["query_timeout_ms"],
                max_ids=limits["max_candidates"],
            )
        session_args, cancelled = _session_request(
            self.store, self.profile, self.session
        )
        request.update(session_args)
        reply = self.store._call(
            "fallback", "resolver-resolve", request, self.profile, _cancelled=cancelled
        )
        self.total_checks = reply["total_checks"]
        return reply["result"]

    def health(self, entry, candidate, catalog):
        return self.store._call(
            "fallback",
            "resolver-health",
            dict(entry=entry, candidate=candidate, catalog=catalog),
            self.profile,
        )

    def capture(self, row, policy, catalog, check=None):
        if check is not None:
            raise CatabolicError(
                "native fallback capture does not support foreign Python check callbacks"
            )
        request, cancelled = _session_request(self.store, self.profile, self.session)
        request.update(
            row=row,
            policy=policy,
            catalog=catalog,
            operation_id=self.operation_id,
            primary_choices=self.primary_choices,
            total_checks=self.total_checks,
        )
        return self.store._call(
            "fallback", "resolver-capture", request, self.profile, _cancelled=cancelled
        )
