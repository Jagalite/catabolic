# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Isolated HTTP/Apprise delivery. Only a constant, sanitized result escapes."""

import contextlib
import io
import json
import logging
import sys
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener

from .callback_urls import origin
from .store import encode

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


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def validate_response(response, contracts):
    contract = contracts.get(
        str(response.status), contracts.get("2XX", contracts.get("default"))
    )
    if contract is None:
        return "unexpected_response"
    length = response.headers.get("Content-Length")
    if length is not None:
        try:
            length = int(length)
        except ValueError:
            return "invalid_response"
        if length < 0 or length > 65536 or (not contract["has_body"] and length):
            return "invalid_response"
    raw = response.read(65537)
    if len(raw) > 65536:
        return "invalid_response"
    if not contract["has_body"]:
        return "complete" if not raw else "invalid_response"
    if (
        response.headers.get_content_type() != "application/json"
        or response.headers.get("Content-Encoding", "identity").lower() != "identity"
    ):
        return "invalid_response"
    try:
        from .openapi_operations import validator

        def reject_constant(value):
            raise ValueError("invalid_json_number")

        def unique_object(pairs):
            result = {}
            for key, value in pairs:
                if key in result:
                    raise ValueError("duplicate_json_key")
                result[key] = value
            return result

        body = json.loads(
            raw.decode("utf-8"),
            parse_constant=reject_constant,
            object_pairs_hook=unique_object,
        )
        encode(body)  # Reject finite JSON tokens that overflow Python floats.
        return (
            "complete"
            if validator(contract["schema"]).is_valid(body)
            else "invalid_response"
        )
    except (ValueError, RecursionError):
        return "invalid_response"


def webhook(payload):
    try:
        origin(payload["url"])
        envelope = payload.get("envelope")
        if not isinstance(envelope, dict) or not envelope.get("id"):
            return "invalid_destination"
        custom = payload.get("request")
        headers = {
            "Content-Type": "application/json",
            "Idempotency-Key": envelope["id"],
            "User-Agent": "Catabolic-Webhook/1",
        }
        if custom is not None:
            headers.update(custom.get("headers", {}))
        request = Request(
            payload["url"],
            data=(encode(custom["body"]).encode() if custom["has_body"] else None)
            if custom is not None
            else json.dumps(envelope).encode(),
            method=custom["method"] if custom is not None else "POST",
            headers=headers,
        )
        with build_opener(NoRedirect(), ProxyHandler({})).open(
            request, timeout=20
        ) as response:
            if not 200 <= response.status < 300:
                return "temporarily_failed"
            return (
                validate_response(response, custom["responses"])
                if custom is not None
                else "complete"
            )
    except (ValueError, TypeError):
        return "invalid_destination"
    except OSError:
        return "temporarily_failed"


def notify(payload):
    try:
        scheme = urlsplit(payload["url"]).scheme
    except (TypeError, ValueError):
        return "invalid_destination"
    if scheme in ("http", "https"):
        return webhook(payload)

    if scheme not in ALLOWED or payload["event"] not in MESSAGES:
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
        raw = sys.stdin.buffer.read(262145)
        if len(raw) > 262144:
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
