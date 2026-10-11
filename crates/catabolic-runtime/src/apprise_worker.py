# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Retained optional Apprise adapter. No catalog or HTTP webhook logic lives here.

Derived from src/catabolic/notification_worker.py at frozen reference commit
453fca983222c6665a48775eef67c489c96b527e (MIT, Catabolic Contributors).
Preserves its allowlisted protocols, static summaries and Apprise call contract.
The native runtime embeds this adapter and runs it in an isolated interpreter
selected by CATABOLIC_APPRISE_PYTHON (default python3), only for optional Apprise.
"""

import contextlib
import io
import json
import logging
import sys
from urllib.parse import urlsplit

ALLOWED = {
    "ntfy",
    "ntfys",
    "discord",
    "mailto",
    "mailtos",
    "tgram",
    "gotify",
    "gotifys",
}
MESSAGES = {
    "job_completed": "A Catabolic processing job completed.",
    "job_failed": "A Catabolic processing job failed.",
    "fallback_selected": "A Catabolic fallback was selected.",
    "fallback_unresolved": "A Catabolic fallback remains unresolved.",
    "projection_updated": "A Catabolic projection update was locally verified.",
    "scan_requested": "A consumer accepted a library scan request. Indexing has not been verified.",
    "scan_failed": "A library scan request could not be delivered. Retry the consumer delivery after repair.",
    "consumer_needs_repair": "A Catabolic consumer integration needs configuration or permission repair.",
}


def notify(payload):
    if (
        urlsplit(payload["url"]).scheme not in ALLOWED
        or payload["event"] not in MESSAGES
    ):
        return "invalid_destination"
    try:
        from apprise import Apprise, AppriseAsset
    except ImportError:
        return "uninstalled"
    obj = Apprise(asset=AppriseAsset(async_mode=False))
    if not obj.add(payload["url"]):
        return "invalid_destination"
    return (
        "complete"
        if obj.notify(
            title="Catabolic",
            body=MESSAGES[payload["event"]],
            notify_type={"info": "info", "warning": "warning", "error": "failure"}[
                payload["severity"]
            ],
        )
        else "temporarily_failed"
    )


logging.disable(logging.CRITICAL)
status = "temporarily_failed"
try:
    raw = sys.stdin.buffer.read(16385)
    if len(raw) <= 16384:
        with (
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            status = notify(json.loads(raw))
except Exception:
    pass
print(json.dumps({"status": status}))
