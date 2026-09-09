# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

from typing import Literal

from pydantic import BaseModel, ConfigDict, Field


class Model(BaseModel):
    model_config = ConfigDict(extra="forbid", strict=True)


class SQL(Model):
    query: str = Field(min_length=1, max_length=1048576)
    params: dict = Field(default_factory=dict)
    limit: int = Field(default=100, ge=1, le=1000)


class GraphQL(Model):
    query: str = Field(min_length=1, max_length=65536)
    variables: dict = Field(default_factory=dict)
    operationName: str | None = None


class Demand(Model):
    item_id: str
    source_file_id: str
    source_revision: str
    operation_id: str


class Ticket(Model):
    file_id: str
    revision: str
    ttl: int = Field(default=300, ge=1, le=3600)
    max_bytes: int = Field(default=1073741824, ge=1, le=107374182400)


class Resolve(Model):
    file_id: str | None = None
    definition_id: str | None = None


class Definition(Model):
    definition: dict
    expected_plan: str | None = None
    apply: bool = False


class Page(Model):
    limit: int = Field(default=100, ge=1, le=1000)
    cursor: str | None = None


class RenditionResult(Model):
    rendition_id: str
    file_id: str
    revision: str
    content_path: str


class DemandStatus(Model):
    request_id: str
    state: Literal[
        "queued",
        "running",
        "validating",
        "ready",
        "blocked",
        "failed",
        "cancelled",
        "stale",
    ]
    status_url: str
    result: RenditionResult | None
    progress: float | None
    blockers: list[str]


class MetadataPage(Model):
    data: list[dict]
    page_complete: bool
    has_more: bool
    selection_complete: bool
    next_cursor: str | None


class EventPage(Model):
    events: list[dict]
    cursor: str
    page_complete: bool
