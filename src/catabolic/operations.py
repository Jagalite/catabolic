# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Versioned definitions; execution remains owned by processing/artifacts/processors."""

import hashlib
import json
from uuid import uuid4

from .curation import bounded_rows, page_limit
from .domain import CatabolicError, name
from .store import encode


class Operations:
    def __init__(self, app):
        self.app, self.store = app, app.store

    def get(self, identifier):
        rows = self.store.rows(
            "SELECT * FROM processing_recipes WHERE id=?", (identifier,)
        )
        if not rows:
            raise CatabolicError("unknown immutable operation revision")
        return {**rows[0], "definition": json.loads(rows[0]["definition"])}

    def list(self, limit=100, after=""):
        page_limit(limit)
        rows, more = bounded_rows(
            self.store,
            "SELECT * FROM processing_recipes WHERE id>? ORDER BY id LIMIT ?",
            (after, limit + 1),
            limit,
        )
        return {
            "operations": [
                {**r, "definition": json.loads(r["definition"])} for r in rows
            ],
            "next_after": rows[-1]["id"] if more else None,
        }

    def put(self, operation_name, value):
        from .artifacts import Artifacts
        from .processing import operation_config

        self.app.require_recovered()
        name(operation_name)
        if (
            not isinstance(value, dict)
            or set(value)
            - {"version", "kind", "operation", "options", "output_definition_id"}
            or value.get("version", 1) != 1
            or type(value.get("version", 1)) is not int
        ):
            raise CatabolicError("invalid version 1 operation definition")
        kind = value.get("kind")
        if not isinstance(value.get("operation"), str) or not isinstance(
            value.get("options", {}), dict
        ):
            raise CatabolicError(
                "operation must be a name and options must be an object"
            )
        if kind == "render":
            return Artifacts(self.app).recipe(
                operation_name,
                value.get("operation"),
                value.get("options"),
                value.get("output_definition_id"),
            )
        if kind == "external":
            from .outputs import Outputs
            from .processors import Processors

            Processors(self.app).get(value.get("operation"))
            Outputs(self.app).get_definition(value.get("output_definition_id"))
            if (
                not isinstance(value.get("options", {}), dict)
                or len(encode(value.get("options", {})).encode()) > 65536
            ):
                raise CatabolicError(
                    "external configuration must be an object bounded to 64 KiB"
                )
        elif kind != "analysis" or "output_definition_id" in value:
            raise CatabolicError(
                "analysis operations record facts; render operations produce files"
            )
        if kind == "analysis":
            operation_config(value.get("operation"), value.get("options"))
        output_id = value.get("output_definition_id")
        value = {
            "version": 1,
            "kind": kind,
            "operation": value["operation"],
            "options": value.get("options", {}),
        }
        if kind == "external":
            value["output_definition_id"] = output_id
        serialized = encode(value)
        digest = hashlib.sha256(serialized.encode()).hexdigest()
        with self.store.transaction() as db:
            old = db.execute(
                "SELECT id FROM processing_recipes WHERE name=? AND digest=?",
                (operation_name, digest),
            ).fetchone()
            if old:
                identifier = old[0]
            else:
                identifier = str(uuid4())
                revision = db.execute(
                    "SELECT coalesce(max(revision),0)+1 FROM processing_recipes WHERE name=?",
                    (operation_name,),
                ).fetchone()[0]
                db.execute(
                    "INSERT INTO processing_recipes(id,name,revision,preset,definition,digest,operation_kind,output_definition_id) VALUES (?,?,?,?,?,?,?,?)",
                    (
                        identifier,
                        operation_name,
                        revision,
                        value["operation"],
                        serialized,
                        digest,
                        kind,
                        output_id,
                    ),
                )
        return self.get(identifier)


def capabilities():
    from .processing import OPERATIONS
    from .rendering import PRESETS

    return {
        "version": 1,
        "kinds": {
            "analysis": {
                "operations": list(OPERATIONS),
                "result": "file_facts and durable job evidence",
                "option_limits": {
                    "timeout": [1, 86400],
                    "max_bytes": [1, 8388608],
                    "analysis_bytes": [1, 67108864],
                    "analysis_us": [1, 60000000],
                },
                "text_backends": {
                    "utf8": {"formats": ["txt", "md", "srt", "vtt"], "default": True},
                    "pdf": {
                        "tool": "pdftotext",
                        "coverage": "embedded text",
                        "max_pages": [1, 1000],
                        "default_max_pages": 100,
                    },
                    "ocr": {
                        "tool": "tesseract",
                        "formats": ["png", "jpeg"],
                        "language": "one to four three-letter codes joined with +; default eng",
                        "coverage": "one raster image; recognition accuracy is not verified",
                    },
                },
            },
            "external": {
                "operation": "immutable processor name",
                "result": "validated receipt and media output",
                "execution": "processor tick or claim/dispatch/complete; rule run reports state",
                "configuration": "bounded JSON options and immutable output_definition_id",
            },
            "render": {
                "operations": list(PRESETS),
                "result": "validated artifact, file occurrence and media output",
                "capabilities_command": "artifact capabilities",
            },
        },
        "external": {
            "commands": "processor and rendition import-receipt",
            "result": "validated receipt and media output",
        },
        "execution": "bounded planning and explicit execution; no recursive rule dispatch",
    }
