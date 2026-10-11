"""Immutable fallback policy revisions on the caller's native Store."""


class Policies:
    def __init__(self, store, profile="default"):
        self.store, self.profile = store, profile

    def get(self, identifier):
        return self.store._call(
            "fallback_policies", "get", dict(id=identifier), self.profile
        )

    def put(self, policy_name, definition):
        return self.store._call(
            "fallback_policies",
            "put",
            dict(name=policy_name, definition=definition),
            self.profile,
        )

    def listing(self, limit=100, after=""):
        return self.store._call(
            "fallback_policies", "list", dict(limit=limit, after=after), self.profile
        )
