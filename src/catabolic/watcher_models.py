# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

from typing import Literal
from zoneinfo import ZoneInfo

from pydantic import Field, model_validator

from .fallback_models import Model
from .plans import Plan
from .watcher_schedule import fields


class Schedule(Model):
    kind: Literal["manual", "interval", "cron"] = "manual"
    seconds: int = Field(default=60, ge=1, le=31536000)
    cron: str = "0 * * * *"
    timezone: str = "UTC"

    @model_validator(mode="after")
    def valid(self):
        ZoneInfo(self.timezone)
        if self.kind == "cron":
            fields(self.cron)
        return self


class WatcherDefinition(Model):
    version: Literal[1] = 1
    authority: Literal["local_owner"] = "local_owner"
    plan: Plan
    schedule: Schedule = Field(default_factory=Schedule)
    events: bool = False
    debounce_seconds: int = Field(default=5, ge=0, le=3600)
    maximum_delay_seconds: int = Field(default=60, ge=1, le=86400)
    max_age_seconds: int = Field(default=60, ge=0, le=86400)
    require_complete_inventory: bool = False
    observe_after_request: bool = False
    reaction: Literal["report", "event", "projection", "processing"] = "report"
    operation_id: str | None = None
    destination: str | None = None
    max_jobs: int = Field(default=10, ge=1, le=100)
    max_changes: int = Field(default=100, ge=0, le=10000)
    max_removals: int = Field(default=0, ge=0, le=10000)
    retry_seconds: int = Field(default=30, ge=1, le=86400)

    @model_validator(mode="after")
    def reaction_contract(self):
        if self.reaction == "projection" and self.plan.kind != "projection":
            raise ValueError("projection reaction requires a pinned projection plan")
        if self.reaction == "processing" and (
            not self.operation_id or self.plan.kind == "projection"
        ):
            raise ValueError(
                "processing requires an approved operation and a selection/fallback plan"
            )
        return self
