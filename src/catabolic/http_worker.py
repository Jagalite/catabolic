# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Private HTTP worker; its parent enforces an overall process deadline.

Credentials travel over stdin, never command-line arguments or error output.
Keeping DNS and TLS inside this process makes their lifetime cancellable too.
"""

import base64
import http.client
import json
import sys
import urllib.error
import urllib.request

from .domain import CatabolicError


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise CatabolicError(
            "redirect refused; configure the final endpoint explicitly"
        )


def fetch(payload):
    req = urllib.request.Request(
        payload["url"],
        headers=payload["headers"],
        method=payload["method"],
        data=base64.b64decode(payload["body"], validate=True)
        if payload.get("body") is not None
        else None,
    )
    opener = urllib.request.build_opener(NoRedirect, urllib.request.ProxyHandler({}))
    try:
        with opener.open(req, timeout=payload["timeout"]) as response:
            raw = response.read(payload["maximum"] + 1)
            if len(raw) > payload["maximum"]:
                raise CatabolicError("HTTP response exceeds byte limit")
            return {"body": base64.b64encode(raw).decode("ascii")}
    except urllib.error.HTTPError as exc:
        if payload.get("structured_errors"):
            return {
                "error": "HTTP request rejected",
                "status": exc.code,
                "retry_after": exc.headers.get("Retry-After", "")[:100],
            }
        raise CatabolicError(f"HTTP request failed with status {exc.code}") from None
    except (urllib.error.URLError, OSError, http.client.HTTPException, ValueError):
        raise CatabolicError(
            "HTTP request failed; check endpoint, credentials and network"
        ) from None


def main():
    try:
        raw = sys.stdin.buffer.read(256 * 1024 + 1)
        if len(raw) > 256 * 1024:
            raise CatabolicError("HTTP request exceeds input limit")
        response = fetch(json.loads(raw))
    except CatabolicError as exc:
        response = {"error": str(exc)}
    except Exception:
        # Neither malformed requests nor unexpected urllib errors may echo secrets.
        response = {"error": "HTTP worker failed"}
    sys.stdout.write(json.dumps(response))


if __name__ == "__main__":
    main()
