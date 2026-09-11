# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Bounded five-field numeric cron; coalesced UTC scheduling, explicit timezone."""

from datetime import datetime, timedelta, timezone
from zoneinfo import ZoneInfo

from .domain import CatabolicError


def fields(expression):
    tokens = expression.split()
    if len(tokens) != 5:
        raise CatabolicError("cron requires minute hour day month weekday")
    result = []
    for token, low, high in zip(
        tokens, (0, 0, 1, 1, 0), (59, 23, 31, 12, 6), strict=True
    ):
        values = set()
        for part in token.split(","):
            base, separator, step = part.partition("/")
            stride = int(step) if separator and step.isdigit() else 1
            if separator and (not step.isdigit() or stride < 1):
                raise CatabolicError("invalid cron step")
            if base == "*":
                start, end = low, high
            elif "-" in base:
                bounds = base.split("-")
                if len(bounds) != 2 or not all(v.isdigit() for v in bounds):
                    raise CatabolicError("invalid cron range")
                start, end = map(int, bounds)
            elif base.isdigit():
                start = end = int(base)
            else:
                raise CatabolicError("cron uses numeric lists, ranges, steps and *")
            if not low <= start <= end <= high:
                raise CatabolicError("cron field out of range")
            values.update(range(start, end + 1, stride))
        result.append(values)
    return result


def next_due(schedule, now):
    if schedule["kind"] == "manual":
        return 0
    if schedule["kind"] == "interval":
        return now + schedule["seconds"]
    allowed = fields(schedule["cron"])
    zone = ZoneInfo(schedule["timezone"])
    current = datetime.fromtimestamp(now, timezone.utc).replace(second=0, microsecond=0)
    # Two years is a finite admission bound, including leap-day schedules.
    for _ in range(2 * 366 * 24 * 60):
        current += timedelta(minutes=1)
        local = current.astimezone(zone)
        if local.fold:
            continue
        values = (
            local.minute,
            local.hour,
            local.day,
            local.month,
            (local.weekday() + 1) % 7,
        )
        # Both restricted day fields must match (documented AND semantics).
        if all(v in options for v, options in zip(values, allowed, strict=True)):
            return current.timestamp()
    raise CatabolicError("cron has no occurrence within two years")
