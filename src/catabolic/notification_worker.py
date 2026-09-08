# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Private optional Apprise subprocess. Only a constant, sanitized result escapes."""

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


def main():
    status = "temporarily_failed"
    logging.disable(logging.CRITICAL)
    try:
        raw = sys.stdin.buffer.read(16385)
        if len(raw) > 16384:
            raise ValueError()
        with (
            contextlib.redirect_stdout(io.StringIO()),
            contextlib.redirect_stderr(io.StringIO()),
        ):
            status = notify(json.loads(raw))
    except Exception:
        pass
    print(json.dumps({"status": status}))


if __name__ == "__main__":
    main()
