"""Fallback projections retain native evaluation and sealed prepared-plan owners."""

import json

from . import _native
from .fallback_resolution import _session_request


def binding(store, profile, catalog):
    return store._call("fallback", "binding", dict(catalog=catalog), profile)


class _PreparedPlan(dict):
    def __init__(self, store, profile, reply):
        super().__init__(reply["plan"])
        self._store, self._profile, self._token = store, profile, reply["token"]

    def __del__(self):
        if hasattr(self, "_token"):
            try:
                self._store._call(
                    "fallback",
                    "projection-drop",
                    dict(plan_token=self._token),
                    self._profile,
                )
            except Exception:
                pass


class FallbackProjection:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def bind(self, catalog, policy_id, query_id, layout):
        return self.store._call(
            "fallback",
            "projection-bind",
            dict(
                catalog=catalog, policy_id=policy_id, query_id=query_id, layout=layout
            ),
            self.profile,
        )

    def plan(self, catalog, *, manual_failback=False, session=None):
        request, cancelled = _session_request(self.store, self.profile, session)
        request.update(catalog=catalog, manual_failback=bool(manual_failback))
        reply = self.store._call(
            "fallback", "projection-plan", request, self.profile, _cancelled=cancelled
        )
        return _PreparedPlan(self.store, self.profile, reply)

    def run(
        self,
        catalog,
        *,
        apply=False,
        expected_plan=None,
        max_removals=0,
        max_changes=100,
        manual_failback=False,
        automatic=False,
    ):
        return self.store._call(
            "fallback",
            "projection-run",
            dict(
                catalog=catalog,
                apply=bool(apply),
                expected_plan=expected_plan,
                max_removals=max_removals,
                max_changes=max_changes,
                manual_failback=bool(manual_failback),
                automatic=bool(automatic),
            ),
            self.profile,
        )

    def apply_prepared(self, plan, *, max_removals=0, max_changes=100):
        return self.store._call(
            "fallback",
            "projection-apply",
            dict(
                plan=plan,
                plan_token=getattr(plan, "_token", None),
                plan_owner_matches=isinstance(plan, _PreparedPlan)
                and plan._store is self.store
                and plan._profile == self.profile,
                max_removals=max_removals,
                max_changes=max_changes,
            ),
            self.profile,
        )

    @staticmethod
    def present(plan):
        return json.loads(_native.fallback_present(json.dumps(plan, allow_nan=False)))
