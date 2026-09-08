# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Discoverable definition shapes; use-case validators also check catalog references."""


def definition_schema(kind):
    from .processing import OPERATIONS
    from .rendering import PRESETS

    text = {"type": "string", "minLength": 1}
    obj = {"type": "object"}
    if kind == "operation":
        variants = []
        for operation_kind in ("analysis", "render", "external"):
            properties = {
                "version": {"const": 1},
                "kind": {"const": operation_kind},
                "operation": text
                if operation_kind == "external"
                else {
                    "enum": list(
                        OPERATIONS if operation_kind == "analysis" else PRESETS
                    )
                },
                "options": obj,
            }
            if operation_kind != "analysis":
                properties["output_definition_id"] = text
            variants.append(
                {
                    "type": "object",
                    "additionalProperties": False,
                    "required": ["kind", "operation"]
                    + (
                        ["output_definition_id"] if operation_kind == "external" else []
                    ),
                    "properties": properties,
                }
            )
        schema = {
            "oneOf": variants,
            "$comment": "Options are operation-specific and budgeted. operation types describes analysis limits; artifact capabilities checks installed render support. Semantic validation resolves immutable output definitions and profile-scoped processors.",
        }
    elif kind == "query":
        properties = {
            "version": {"const": 1},
            "mode": {"enum": ["selection", "rows", "document"]},
            "entity": {"enum": ["item_id", "file_id", "association_id"]},
            "max_ids": {"type": "integer", "minimum": 1, "maximum": 100000},
            "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 60000},
            "selection": {
                "type": "object",
                "required": ["language", "query"],
                "additionalProperties": False,
                "properties": {
                    "language": {"enum": ["sql", "graphql"]},
                    "query": text,
                    "profile": text,
                    "params": obj,
                    "variables": obj,
                    "max_ids": {"type": "integer", "minimum": 1, "maximum": 100000},
                    "page_size": {"type": "integer", "minimum": 1, "maximum": 9999},
                    "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 60000},
                },
            },
            "combine": {"enum": ["union", "intersection", "difference"]},
            "queries": {"type": "array", "minItems": 2, "maxItems": 16, "items": text},
        }
        schema = {
            "type": "object",
            "additionalProperties": False,
            "properties": properties,
            "oneOf": [
                {
                    "required": ["selection"],
                    "not": {
                        "anyOf": [{"required": ["combine"]}, {"required": ["queries"]}]
                    },
                },
                {
                    "required": ["combine", "queries"],
                    "not": {"required": ["selection"]},
                },
            ],
            "$comment": "Shape validation is not selection completeness. SQL/GraphQL language contracts, compatible modes, profiles, revision references, execution budgets and returned IDs are validated by the query owner.",
        }
    elif kind == "projection":
        schema = {
            "type": "object",
            "additionalProperties": False,
            "required": ["catalog", "query_id", "layout"],
            "properties": {
                "catalog": text,
                "query_id": text,
                "layout": text,
                "copies": obj,
                "renditions": obj,
            },
            "$comment": "Configure with projection put flags. Catalog and layout must exist; query must be a selection. Policies use the existing copy-selection and rendition-publication validators. Layouts are inspectable with layout presets/show.",
        }
    else:
        raise ValueError("unknown programming contract")
    return {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Catabolic " + kind + " input shape v1",
        **schema,
    }
