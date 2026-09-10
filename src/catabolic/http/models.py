# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

from typing import Generic, Literal, TypeVar

from pydantic import BaseModel, ConfigDict, Field, JsonValue


class Model(BaseModel):
    model_config = ConfigDict(extra="forbid", strict=True)


class SQL(Model):
    query: str = Field(min_length=1, max_length=1048576)
    params: dict[str, str | int | float | bool | None] = Field(default_factory=dict)
    limit: int = Field(default=100, ge=1, le=1000)


class GraphQL(Model):
    query: str = Field(min_length=1, max_length=65536)
    variables: dict[str, JsonValue] = Field(default_factory=dict)
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
    definition: dict[str, JsonValue]
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


RequestState = Literal[
    "queued",
    "running",
    "validating",
    "ready",
    "blocked",
    "failed",
    "cancelled",
    "stale",
]


class DemandStatus(Model):
    request_id: str
    state: RequestState
    status_url: str
    result: RenditionResult | None
    progress: float | None
    blockers: list[str]


T = TypeVar("T")


class MetadataPage(Model, Generic[T]):
    data: list[T]
    page_complete: bool
    has_more: bool
    selection_complete: bool
    next_cursor: str | None


class Item(Model):
    id: str
    kind: str
    metadata: dict[str, JsonValue]


class File(Model):
    id: str
    revision: str
    size: str | None = Field(
        default=None, description="Decimal byte count; lossless in JavaScript."
    )
    mtime_ns: str | None = Field(
        default=None, description="Decimal Unix nanoseconds; lossless in JavaScript."
    )
    status: str | None = None
    location: str | None = Field(
        default=None, description="Present only for authorized operators."
    )
    path: str | None = Field(
        default=None, description="Present only for authorized operators."
    )


class Rendition(Model):
    id: str
    file_id: str
    definition_id: str
    origin: str
    revision: str


class Identity(Model):
    principal: str
    profile: str
    actions: list[str]
    expires: float = Field(description="Unix seconds, including fractional seconds.")


class CapabilityLimits(Model):
    page_size: int
    streams: int
    streams_per_principal: int
    stream_buffer_bytes: int
    requests_per_minute: int


class ContentCapabilities(Model):
    ranges: Literal["single"]
    etag: Literal["weak_revision"]
    in_place_mutation: Literal["abort_remaining_transfer"]


class Capabilities(Model):
    version: Literal[1]
    schema_: int = Field(alias="schema")
    read_only: bool
    processing_admission: bool
    worker_ready: bool
    operations: list[str]
    limits: CapabilityLimits
    content: ContentCapabilities


class SQLResult(Model):
    interface_version: int
    profile: str
    columns: list[str]
    rows: list[list[JsonValue]]
    row_count: int
    max_rows: int
    truncated: bool
    truncation_reason: str | None
    complete: bool


class GraphQLLocation(Model):
    line: int
    column: int


class GraphQLError(Model):
    message: str
    locations: list[GraphQLLocation] | None = None
    path: list[str | int] | None = None
    extensions: dict[str, JsonValue] | None = None


class GraphQLResult(Model):
    data: dict[str, JsonValue] | None = None
    errors: list[GraphQLError] | None = None
    extensions: dict[str, JsonValue] | None = None


class SelectionResult(Model):
    mode: Literal["selection"]
    entity: Literal["item_id", "file_id", "association_id"]
    ids: list[str]
    page_complete: bool
    has_more: bool
    selection_complete: bool
    next_cursor: str | None


class Snapshot(Model):
    snapshot_id: str
    expires: float
    selection_complete: bool


class ResolvedContent(Model):
    file_id: str
    revision: str
    content_path: str


class ProjectionEntry(Model):
    id: str
    item_id: str
    file_id: str
    path: str


class ProjectionPage(Model):
    data: list[ProjectionEntry]
    page_complete: bool
    has_more: bool
    next_cursor: str | None


class ContentTicket(Model):
    ticket_id: str
    expires: float
    content_path: str


class Revocation(Model):
    revoked: bool


class RequestChanged(Model):
    type: Literal["request.changed"]
    request_id: str
    state: RequestState


class EventPage(Model):
    events: list[RequestChanged]
    cursor: str
    page_complete: bool


class SavedQuery(Model):
    id: str
    profile: str
    name: str
    revision: int
    definition: dict[str, JsonValue]
    digest: str
    created_at: str


class Rule(Model):
    id: str
    profile: str
    name: str
    revision: int
    recipe_id: str
    location: str | None
    selection: dict[str, JsonValue]
    estimate_options: dict[str, JsonValue]
    required: bool
    enabled: bool
    digest: str
    allow_derived: int
    query_id: str | None
    created_at: str


class CopyPreference(Model):
    field: str
    order: Literal["asc", "desc"] | None = None
    values: list[JsonValue] | None = None


class CopyPolicy(Model):
    require: dict[str, JsonValue] = Field(default_factory=dict)
    prefer: list[CopyPreference] = Field(default_factory=list)
    tie_break: Literal["file_id"] | None = None
    available_only: bool = False


class RenditionPolicy(Model):
    version: Literal[1]
    mode: Literal["all", "preferred"]
    include_originals: bool
    purpose: str | None = None
    definition_id: str | None = None
    rule_id: str | None = None


class RefreshSetting(Model):
    profile: str
    catalog: str
    enabled: Literal[0, 1]
    max_removals: int


class Projection(Model):
    profile: str
    catalog: str
    query_id: str | None
    layout: str | None
    legacy: bool
    copy_policy: CopyPolicy | None
    rendition_policy: RenditionPolicy | None
    applied_layout: str | None
    refresh: list[RefreshSetting]


class OperatorPreview(Model):
    applied: Literal[False]
    plan_id: str
    definition: dict[str, JsonValue]


class OperatorApplied(Model):
    applied: Literal[True]
    result: SavedQuery | Rule | Projection


class Problem(Model):
    type: str
    title: str
    status: int
    code: str
    request_id: str
    retryable: bool
    remediation: str
