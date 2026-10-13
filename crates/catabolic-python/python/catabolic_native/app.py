"""Native-owned Application facade. Python only adapts arguments and values."""

import json

from .media_catalog import MediaCatalog
from .query import CatalogQuery
from .store import Store
from .tagging import Tagging


class BoundRoot(dict):
    """Binding mapping with native runtime identity evidence attached."""

    def __init__(self, value, volume_uuid, policy):
        super().__init__(value)
        self.volume_uuid, self.policy = volume_uuid, policy
        self.trust_path = all(v == "skip" for v in policy["settings"].values())
        self.evidence = {"policy": policy, "checks": {}, "allowed": False}


class Application:
    def __init__(self, store: Store, profile: str = "default"):
        self.store, self.profile = store, profile
        self._call("initialize")
        self.queries = CatalogQuery(store, profile)
        self.media = MediaCatalog(self)
        self.tags = Tagging(self)

    def _call(self, operation, **request):
        return self.store._call("application", operation, request, self.profile)

    def status(self) -> dict:
        return self._call("status")

    def require_recovered(self):
        return self._call("require_recovered")

    def add_profile(self, profile: str) -> dict:
        return self._call("add_profile", profile=profile)

    def link_mode(self, catalog):
        return self._call("link_mode", catalog=catalog)

    def bind(
        self, kind: str, owner: str, root: str | None = None, *, link_mode=None
    ) -> dict:
        return self._call(
            "bind",
            kind=kind,
            owner=owner,
            root=str(root) if root is not None else None,
            link_mode=link_mode,
        )

    def binding(self, kind: str, owner: str) -> dict:
        return BoundRoot(**self._call("binding", kind=kind, owner=owner))

    def scan(
        self,
        location: str | None = None,
        *,
        exclude: list[str] | None = None,
        request_id: str | None = None,
        extended=False,
        budgets=None,
        scopes=None,
        continue_id=None,
    ) -> dict:
        request = dict(
            location=location,
            exclude=exclude,
            request_id=request_id,
            extended=extended,
            budgets=budgets,
            scopes=scopes,
            continue_id=continue_id,
        )
        return json.loads(
            self.store._owner.scan_with_publisher(
                self.profile, json.dumps(request, allow_nan=False), self
            )
        )

    def files(self, **filters) -> dict:
        return self.queries.files(**filters)

    def put_item(
        self,
        kind: str,
        identities: dict[str, str],
        metadata: dict,
        item_id: str | None = None,
        *,
        _db=None,
    ) -> dict:
        if _db is not None:
            self.store._handle(_db)
        return self._call(
            "put_item", kind=kind, identities=identities, metadata=metadata, id=item_id
        )

    def put_mapping(self, catalog: str, file_id: str, item_id: str, path: str) -> dict:
        return self._call(
            "put_mapping", catalog=catalog, file_id=file_id, item_id=item_id, path=path
        )

    def mapping_id(self, catalog, file_id, item_id, path):
        return self._call(
            "mapping_id", catalog=catalog, file_id=file_id, item_id=item_id, path=path
        )

    def disable_mapping(self, mapping_id: str) -> dict:
        return self._call("disable_mapping", id=mapping_id)
