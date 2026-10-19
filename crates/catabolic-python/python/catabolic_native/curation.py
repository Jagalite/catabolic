"""Persistent curation facade; decisions belong to the shared native owner."""


def occurrence(store, profile, file_id):
    return store._call("curation", "occurrence", {"file_id": file_id}, profile)


class Curation:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def _call(self, operation, **request):
        return self.store._call("curation", operation, request, self.profile)

    def snapshot(self, file_id, payload):
        return self._call("snapshot", file_id=file_id, payload=payload)

    def put(self, file_id, payload, *, source="manual", evidence=None):
        return self._call(
            "proposal-put",
            file_id=file_id,
            payload=payload,
            source=source,
            evidence=evidence,
        )

    def get(self, identifier):
        return self._call("proposal-get", id=identifier)

    def accept_existing_association_evidence(self, identifier, *, actor):
        return self._call("proposal-accept-existing", id=identifier, actor=actor)

    def list(self, *, state=None, limit=100, after=""):
        return self._call("proposal-list", state=state, limit=limit, after=after)

    def decide(self, identifier, *, accept, actor):
        return self._call("proposal-decide", id=identifier, accept=accept, actor=actor)

    def history(self, *, limit=100, after=0):
        return self._call("proposal-history", limit=limit, after=after)

    def sidecars(self, *, location=None, limit=100, after="", apply=False):
        return self._call(
            "sidecars", location=location, limit=limit, after=after, apply=apply
        )

    def expected_put(self, collection, definition):
        return self._call("expected-put", collection=collection, value=definition)

    def completeness(self, identifier, *, limit=100, after=0):
        return self._call("completeness", id=identifier, limit=limit, after=after)
