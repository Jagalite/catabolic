"""Thin native-owned public service facade."""


class MediaCatalog:
    def __init__(self, application):
        self.app = application
        self.store = self.app.store

    def associate_in_transaction(
        self,
        db,
        file_id,
        item_id,
        *,
        role="primary",
        part=None,
        clear_part=False,
        metadata=None,
        origin="explicit",
    ):
        self.store._handle(db)
        return self.store._call(
            "media",
            "associate_in_transaction",
            {
                "file_id": file_id,
                "item_id": item_id,
                "role": role,
                "part": part,
                "clear_part": clear_part,
                "metadata": metadata,
                "origin": origin,
            },
            self.app.profile,
        )

    def associate(
        self,
        file_id,
        item_id,
        *,
        role="primary",
        part=None,
        clear_part=False,
        metadata=None,
    ):
        return self.store._call(
            "media",
            "associate",
            {
                "file_id": file_id,
                "item_id": item_id,
                "role": role,
                "part": part,
                "clear_part": clear_part,
                "metadata": metadata,
            },
            self.app.profile,
        )

    def disable_association(self, identifier):
        return self.store._call(
            "media", "association-disable", {"id": identifier}, self.app.profile
        )

    def relate(
        self,
        source,
        target,
        kind,
        *,
        position=None,
        clear_position=False,
        metadata=None,
        _db=None,
    ):
        if _db is not None:
            self.store._handle(_db)
        return self.store._call(
            "media",
            "relate",
            {
                "source": source,
                "target": target,
                "kind": kind,
                "position": position,
                "clear_position": clear_position,
                "metadata": metadata,
            },
            self.app.profile,
        )

    def disable_relationship(self, identifier):
        return self.store._call(
            "media", "relationship-disable", {"id": identifier}, self.app.profile
        )
