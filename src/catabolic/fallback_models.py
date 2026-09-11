# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Shared immutable fallback definition and decision contracts."""

from typing import Literal

from pydantic import BaseModel, ConfigDict, Field, JsonValue


class Model(BaseModel):
    model_config = ConfigDict(extra="forbid", strict=True)


class Tier(Model):
    name: str = Field(min_length=1, max_length=128)
    query_id: str = Field(min_length=1, max_length=256)


class Preference(Model):
    field: str
    order: Literal["asc", "desc"] | None = None
    values: list[JsonValue] | None = None


class Ranking(Model):
    require: dict[str, JsonValue] = Field(default_factory=dict)
    prefer: list[Preference] = Field(default_factory=list, max_length=20)
    tie_break: Literal["file_id"] | None = None


class Failback(Model):
    mode: Literal["immediate", "stable", "manual"] = "stable"
    minimum_healthy_seconds: int = Field(default=300, ge=0, le=86400)
    minimum_successful_checks: int = Field(default=2, ge=1, le=1000)


class Budgets(Model):
    max_entries: int = Field(default=1000, ge=1, le=10000)
    max_candidates: int = Field(default=10000, ge=1, le=100000)
    max_checks: int = Field(default=128, ge=1, le=1000)
    query_timeout_ms: int = Field(default=5000, ge=1, le=30000)
    probe_timeout_ms: int = Field(default=1000, ge=10, le=10000)


class ComponentRequirement(Model):
    name: str = Field(min_length=1, max_length=128)
    fallbacks: list[Tier] = Field(min_length=1, max_length=32)
    required: bool = True


class ComponentPackage(Model):
    requirements: list[ComponentRequirement] = Field(min_length=1, max_length=16)
    publication: Literal["container", "sidecars", "selected_only"] = "container"
    operation_id: str | None = None
    supported_codecs: list[str] | None = Field(default=None, max_length=64)


class Policy(Model):
    version: Literal[1] = 1
    slot_roles: list[str] = Field(
        default_factory=lambda: ["primary"], min_length=1, max_length=8
    )
    fallbacks: list[Tier] = Field(min_length=1, max_length=32)
    within_tier: Ranking = Field(default_factory=Ranking)
    lineage_requirement: Literal[
        "accepted_source_revision", "current_source_revision"
    ] = "current_source_revision"
    failback: Failback = Field(default_factory=Failback)
    on_unavailable: Literal["retain"] = "retain"
    budgets: Budgets = Field(default_factory=Budgets)
    package: ComponentPackage | None = None


class LogicalEntry(Model):
    item_id: str
    role: str = "primary"
    part: int | None = None
    variant: str = ""


class Decision(Model):
    entry_id: str
    item_id: str
    role: str
    part: int | None
    variant: str
    policy_id: str
    state: Literal["resolved_preferred", "resolved_fallback", "unresolved", "blocked"]
    selected_tier: int | None = None
    selected_tier_name: str | None = None
    file_id: str | None = None
    revision: str | None = None
    rendition_id: str | None = None
    content_path: str | None = None
    evidence: dict[str, JsonValue] = Field(default_factory=dict)
    reasons: list[str] = Field(default_factory=list)
