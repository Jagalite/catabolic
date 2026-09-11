# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Authorized context for the existing GraphQL schema and executor."""

from graphql import GraphQLError

from ..graphql_query import QueryContext
from .catalog import Catalog


class AuthorizedContext(QueryContext):
    def __init__(self, store, profile, deadline, access):
        super().__init__(store, profile, deadline)
        self.access = access
        self.catalog_access = Catalog(access)

    def resolve(self, source, info, /, **args):
        parent, field = info.parent_type.name, info.field_name
        if not self.access.operator("metadata:read"):
            allowed = {
                "Query": {
                    "items",
                    "item",
                    "files",
                    "file",
                    "associations",
                    "relationships",
                    "mappings",
                    "catalogs",
                    "profile",
                    "schemaVersion",
                    "mediaTypes",
                    "fallbackPolicy",
                    "components",
                    "componentOccurrences",
                    "fallbackResolutions",
                },
                "Component": {"id", "kind", "itemIds"},
                "ComponentOccurrence": {
                    "id",
                    "componentId",
                    "itemId",
                    "fileId",
                    "kind",
                    "role",
                    "language",
                    "title",
                    "codec",
                    "defaultFlag",
                    "forced",
                    "commentary",
                    "hearingImpaired",
                    "visualImpaired",
                    "storage",
                    "locator",
                    "revision",
                    "current",
                    "technicallyVerified",
                    "conflicts",
                },
                "Item": {
                    "id",
                    "kind",
                    "title",
                    "year",
                    "metadata",
                    "associations",
                    "relationships",
                    "mappings",
                },
                "File": {"id", "size", "mtimeNs", "status", "associations", "mappings"},
                "Association": {
                    "id",
                    "fileId",
                    "itemId",
                    "role",
                    "part",
                    "active",
                    "item",
                    "file",
                },
                "Relationship": {
                    "id",
                    "sourceId",
                    "targetId",
                    "kind",
                    "position",
                    "active",
                    "source",
                    "target",
                },
                "Mapping": {
                    "id",
                    "catalog",
                    "path",
                    "active",
                    "itemId",
                    "fileId",
                    "item",
                    "file",
                },
                "Catalog": {"id", "mappings"},
            }
            if parent in allowed and field not in allowed[parent]:
                raise GraphQLError(
                    "field is not authorized", extensions={"code": "FORBIDDEN"}
                )
            # Evidence-backed filters must not inspect data outside the admitted views.
            if any(
                args.get(k) not in (None, False, [])
                for k in (
                    "hasRendition",
                    "missingRendition",
                    "curationStatus",
                    "tags",
                    "anyTags",
                    "notTags",
                    "descendants",
                )
            ):
                raise GraphQLError(
                    "filter is not authorized", extensions={"code": "FORBIDDEN"}
                )
        if (
            parent == "Query"
            and field == "fallbackPolicy"
            and not any(
                g.get("operator") or args["id"] in g.get("fallback_policy_ids", [])
                for g in self.access.matching("metadata:read")
            )
        ):
            raise GraphQLError(
                "resource is not authorized", extensions={"code": "FORBIDDEN"}
            )
        if args.get("after"):
            args["after"] = self.catalog_access.after(
                [parent, field, source.get("id") if source else None], args["after"]
            )
        if parent == "Query" and field == "files" and not args.get("unmapped"):
            args["catalog"] = None
        value = super().resolve(source, info, **args)
        if (
            isinstance(value, dict)
            and isinstance(value.get("pageInfo"), dict)
            and value["pageInfo"].get("endCursor")
        ):
            value = {
                **value,
                "pageInfo": {
                    **value["pageInfo"],
                    "endCursor": self.catalog_access.cursor(
                        [parent, field, source.get("id") if source else None],
                        value["pageInfo"]["endCursor"],
                    ),
                },
            }
        return value
