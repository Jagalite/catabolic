"""Thin public adapter for the persistent native scan publication owner."""

import json
import sqlite3

from .store import CatabolicError


class PublicationStopped(RuntimeError):
    pass


class Publisher:
    def __init__(self, app, job, binding):
        self.app, self.job, self.binding = app, job, binding
        self._owner = app.store._owner.publisher(
            app.profile, json.dumps(job, allow_nan=False)
        )
        self.statistics = {}

    @classmethod
    def _from_native(cls, app, owner, job, binding):
        from .app import BoundRoot

        value = cls.__new__(cls)
        value.app, value.job, value.binding = (
            app,
            json.loads(job),
            BoundRoot(**json.loads(binding)),
        )
        value._owner, value.statistics = owner, {}
        return value

    def _call(self, operation, value=None):
        return json.loads(
            self._owner.call(operation, json.dumps(value, allow_nan=False))
        )

    @property
    def initial_scopes(self):
        after = 0
        while True:
            page = self._call("initial_scopes", after)
            if not page["rows"]:
                return
            yield from page["rows"]
            after = page["after"]

    @property
    def entries(self):
        return self._call("state")["entries"]

    @property
    def scopes(self):
        return {
            path: tuple(value) for path, value in self._call("state")["scopes"].items()
        }

    @property
    def bytes(self):
        return self._call("state")["bytes"]

    @property
    def sequence(self):
        return self._call("state")["sequence"]

    @property
    def blocker(self):
        return self._call("state")["blocker"]

    @property
    def budgets(self):
        return self._call("state")["execution"]["budgets"]

    @property
    def extensions(self):
        return self._call("state")["execution"].get("extensions")

    @property
    def include_regex(self):
        return self._call("state")["execution"].get("include_regex", [])

    @property
    def exclude_regex(self):
        return self._call("state")["execution"].get("exclude_regex", [])

    @property
    def root_identity(self):
        value = self._call("state")["root_identity"]
        return None if value is None else tuple(value)

    @root_identity.setter
    def root_identity(self, value):
        self._call("root_identity", value)

    def __call__(self, entry):
        if self._call("entry", entry):
            self.checkpoint()

    def directory(self, path, state, detail):
        if self._call("directory", dict(path=path, state=state, detail=detail)):
            self.flush()
        else:
            self.checkpoint()

    def checkpoint(self):
        if self._call("checkpoint"):
            self.flush()

    def stop(self, reason):
        self._call("stop", reason)

    def flush(self):
        try:
            self._flush()
        except (OSError, CatabolicError, sqlite3.Error) as exc:
            raise PublicationStopped(str(exc)) from exc

    def _flush(self):
        self._call("flush")

    def _directory_json(self, value):
        self.directory(*json.loads(value))

    def _entry_json(self, value):
        self(json.loads(value))
