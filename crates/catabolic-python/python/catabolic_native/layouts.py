"""Declarative layouts forwarded to the caller's persistent native owner."""

import json

from . import _native

# Pinned by migration_rust_resources.py; the native owner uses these same bytes.
PRESETS = json.loads(_native.layout_presets())


class Layouts:
    def __init__(self, app):
        self.app, self.store = app, app.store

    def get(self, identifier):
        return self.store._call(
            "layouts", "get", dict(name=identifier), self.app.profile
        )

    def list(self):
        return self.store._call("layouts", "list", {}, self.app.profile)

    def put(self, identifier, definition):
        return self.store._call(
            "layouts", "put", dict(name=identifier, value=definition), self.app.profile
        )

    def effective_definition(self, identifier, catalog):
        return self.store._call(
            "layouts",
            "effective_definition",
            dict(name=identifier, catalog=catalog),
            self.app.profile,
        )

    def run(
        self,
        identifier,
        catalog="global",
        *,
        apply=False,
        replace_layout=False,
        allow_empty=False,
        limit=100,
    ):
        # Python option objects stay opaque until the native boundary consumes
        # their type/value or invokes their truth callback.
        return json.loads(
            self.store._owner.layout_run(
                json.dumps(dict(name=identifier, catalog=catalog), allow_nan=False),
                self.app.profile,
                apply,
                replace_layout,
                allow_empty,
                limit,
            )
        )
