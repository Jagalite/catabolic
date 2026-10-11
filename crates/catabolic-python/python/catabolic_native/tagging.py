"""Thin native-owned public service facade."""


class Tagging:
    def __init__(self, app):
        self.app = app
        self.store = self.app.store

    def put(self, name, *, description=None):
        return self.store._call(
            "tagging",
            "put",
            {"name": name, "description": description},
            self.app.profile,
        )

    def rename(self, name, new_name):
        return self.store._call(
            "tagging", "rename", {"name": name, "new_name": new_name}, self.app.profile
        )

    def alias(self, name, alias, *, remove=False):
        return self.store._call(
            "tagging",
            "alias",
            {"name": name, "alias": alias, "remove": remove},
            self.app.profile,
        )

    def parent(self, child, parent, *, remove=False):
        return self.store._call(
            "tagging",
            "parent",
            {"child": child, "parent": parent, "remove": remove},
            self.app.profile,
        )

    def assign(
        self,
        names,
        *,
        items=(),
        files=(),
        source="manual",
        confidence=None,
        note="",
        remove=False,
    ):
        return self.store._call(
            "tagging",
            "assign",
            {
                "names": list(names),
                "items": list(items),
                "files": list(files),
                "source": source,
                "confidence": confidence,
                "note": note,
                "remove": remove,
            },
            self.app.profile,
        )
