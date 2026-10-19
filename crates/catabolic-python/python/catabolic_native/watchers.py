"""Durable watchers on the caller's persistent native Store."""


class Watchers:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def get(self, watcher):
        return self.store._call("watchers", "get", {"name": watcher}, self.profile)

    def list(self):
        return self.store._call("watchers", "list", {}, self.profile)

    def put(self, watcher, definition):
        return self.store._call(
            "watchers", "put", {"name": watcher, "definition": definition}, self.profile
        )

    def admit(self, watcher):
        return self.store._call("watchers", "admit", {"name": watcher}, self.profile)

    def enable(self, watcher, enabled=True, *, transfer=False):
        request = {"name": watcher, "transfer": bool(transfer)}
        self.store._call("watchers", "enable_validate", request, self.profile)
        request["_enabled_storage"] = int(enabled)
        return self.store._call(
            "watchers", "enable" if enabled else "disable", request, self.profile
        )

    def require_run(self, watcher, identifier, definition_id):
        return self.store._call(
            "watchers", "require_run",
            {"name": watcher, "identifier": identifier, "definition_id": definition_id},
            self.profile,
        )

    def history(self, watcher):
        return self.store._call("watchers", "history", {"name": watcher}, self.profile)

    def run(self, watcher, *, trigger="manual", now=None):
        request = {"name": watcher, "trigger": trigger}
        if now is not None:
            request["now"] = now
        return self.store._call("watchers", "run", request, self.profile)
