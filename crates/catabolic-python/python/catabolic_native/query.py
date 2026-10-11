"""Thin native-owned public service facade."""

from .store import Store


class CatalogQuery:
    def __init__(self, store: Store, profile: str):
        self.store, self.profile = store, profile
        store._call("application", "initialize", {}, profile)

    def files(
        self,
        *,
        catalog="global",
        unmapped=False,
        unidentified=False,
        search=None,
        location=None,
        status=None,
        item=None,
        kind=None,
        year=None,
        identity=None,
        tags=(),
        any_tags=(),
        not_tags=(),
        descendants=False,
        sort="id",
        descending=False,
        limit=100,
        cursor=None,
    ):
        return self.store._call(
            "query",
            "files",
            {
                "catalog": catalog,
                "unmapped": unmapped,
                "unidentified": unidentified,
                "search": search,
                "location": location,
                "status": status,
                "item": item,
                "kind": kind,
                "year": year,
                "identity": identity,
                "tags": list(tags),
                "any_tags": list(any_tags),
                "not_tags": list(not_tags),
                "descendants": descendants,
                "sort": sort,
                "descending": descending,
                "limit": limit,
                "cursor": cursor,
            },
            self.profile,
        )

    def items(
        self,
        *,
        search=None,
        kind=None,
        year=None,
        identity=None,
        metadata=(),
        catalog=None,
        curation_status=None,
        has_rendition=None,
        missing_rendition=None,
        tags=(),
        any_tags=(),
        not_tags=(),
        descendants=False,
        sort="id",
        descending=False,
        limit=100,
        cursor=None,
    ):
        return self.store._call(
            "query",
            "items",
            {
                "search": search,
                "kind": kind,
                "year": year,
                "identity": identity,
                "metadata": list(metadata),
                "catalog": catalog,
                "curation_status": curation_status,
                "has_rendition": has_rendition,
                "missing_rendition": missing_rendition,
                "tags": list(tags),
                "any_tags": list(any_tags),
                "not_tags": list(not_tags),
                "descendants": descendants,
                "sort": sort,
                "descending": descending,
                "limit": limit,
                "cursor": cursor,
            },
            self.profile,
        )

    def tags(
        self,
        *,
        search=None,
        namespace=None,
        parent=None,
        child=None,
        limit=100,
        cursor=None,
    ):
        return self.store._call(
            "query",
            "tags",
            {
                "search": search,
                "namespace": namespace,
                "parent": parent,
                "child": child,
                "limit": limit,
                "cursor": cursor,
            },
            self.profile,
        )

    def taggings(
        self,
        *,
        tag=None,
        item=None,
        file=None,
        source=None,
        active="active",
        limit=100,
        cursor=None,
    ):
        return self.store._call(
            "query",
            "taggings",
            {
                "tag": tag,
                "item": item,
                "file": file,
                "source": source,
                "active": active,
                "limit": limit,
                "cursor": cursor,
            },
            self.profile,
        )

    def mappings(
        self,
        *,
        catalog="global",
        item=None,
        file=None,
        active="all",
        search=None,
        sort="path",
        descending=False,
        limit=100,
        cursor=None,
    ):
        return self.store._call(
            "query",
            "mappings",
            {
                "catalog": catalog,
                "item": item,
                "file": file,
                "active": active,
                "search": search,
                "sort": sort,
                "descending": descending,
                "limit": limit,
                "cursor": cursor,
            },
            self.profile,
        )

    def associations(
        self,
        *,
        item=None,
        file=None,
        role=None,
        active="active",
        limit=100,
        cursor=None,
    ):
        return self.store._call(
            "query",
            "associations",
            {
                "item": item,
                "file": file,
                "role": role,
                "active": active,
                "limit": limit,
                "cursor": cursor,
            },
            self.profile,
        )

    def relationships(
        self,
        *,
        item=None,
        direction="both",
        kind=None,
        active="active",
        limit=100,
        cursor=None,
    ):
        return self.store._call(
            "query",
            "relationships",
            {
                "item": item,
                "direction": direction,
                "kind": kind,
                "active": active,
                "limit": limit,
                "cursor": cursor,
            },
            self.profile,
        )

    def item(
        self,
        item_id=None,
        *,
        identity=None,
        catalog=None,
        include_disabled=False,
        limit=100,
        cursor=None,
    ):
        return self.store._call(
            "query",
            "item",
            {
                "item_id": item_id,
                "identity": identity,
                "catalog": catalog,
                "include_disabled": include_disabled,
                "limit": limit,
                "cursor": cursor,
            },
            self.profile,
        )
