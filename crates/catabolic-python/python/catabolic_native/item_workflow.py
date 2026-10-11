"""Persistent recorded-evidence workflow facade backed by the native Store."""


class ItemWorkflow:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def _call(self, operation, **request):
        return self.store._call("item_workflow", operation, request, self.profile)

    def get(self, item_id):
        return self._call("get", item_id=item_id)

    def checks(self, item_id, *, limit=100, after=""):
        return self._call("checks", item_id=item_id, limit=limit, after=after)

    def note(self, item_id, body, *, kind="note", actor="user", expected_revision=None):
        return self._call(
            "note",
            item_id=item_id,
            body=body,
            kind=kind,
            actor=actor,
            expected_revision=expected_revision,
        )

    def set_status(
        self, item_id, status, *, note="", actor="user", expected_revision=None
    ):
        return self._call(
            "status",
            item_id=item_id,
            status=status,
            note=note,
            actor=actor,
            expected_revision=expected_revision,
        )

    def log(self, item_id, *, limit=100, after=0):
        return self._call("log", item_id=item_id, limit=limit, after=after)

    def require(
        self,
        item_id,
        kind,
        label,
        *,
        target=None,
        actor="user",
        note="",
        expected_revision=None,
    ):
        return self._call(
            "require",
            item_id=item_id,
            kind=kind,
            label=label,
            target=target,
            actor=actor,
            note=note,
            expected_revision=expected_revision,
        )

    def resolve(
        self, requirement_id, state, *, note, actor="user", expected_revision=None
    ):
        return self._call(
            "resolve",
            requirement_id=requirement_id,
            state=state,
            note=note,
            actor=actor,
            expected_revision=expected_revision,
        )
