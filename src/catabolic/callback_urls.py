# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Shared HTTP callback URL validation, without network access."""

from urllib.parse import urlsplit


def origin(value):
    try:
        if (
            not isinstance(value, str)
            or not 1 <= len(value) <= 4096
            or any(ord(c) <= 32 or ord(c) == 127 for c in value)
        ):
            raise ValueError()
        url = urlsplit(value)
        if (
            url.scheme not in ("http", "https")
            or not url.hostname
            or url.username is not None
            or url.password is not None
            or url.fragment
            or url.port == 0
            or "\\" in value
        ):
            raise ValueError()
        return (
            url.scheme,
            url.hostname.lower(),
            url.port or (443 if url.scheme == "https" else 80),
        )
    except ValueError:
        raise ValueError("invalid_callback_url") from None
