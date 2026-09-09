# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Plex PIN authorization and private local credentials; never opens a catalog."""

import fcntl
import json
import os
import stat
import tempfile
import time
import uuid
from pathlib import Path
from urllib.parse import urlencode

from .consumer_adapters import ConsumerError, text
from .domain import CatabolicError
from .network_adapters import request


def directory(value):
    folder = Path(value).expanduser().absolute()
    try:
        folder.mkdir(mode=0o700, parents=True, exist_ok=True)
        info = folder.lstat()
        if (
            not stat.S_ISDIR(info.st_mode)
            or info.st_uid != os.getuid()
            or info.st_mode & 0o077
        ):
            raise ConsumerError("unsafe_credential_storage")
    except OSError:
        raise ConsumerError("credential_storage_unavailable") from None
    return folder


def read_private(filename):
    try:
        fd = os.open(filename, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, "r") as stream:
            info = os.fstat(stream.fileno())
            if (
                not stat.S_ISREG(info.st_mode)
                or info.st_uid != os.getuid()
                or info.st_mode & 0o077
                or info.st_nlink != 1
            ):
                raise ConsumerError("unsafe_credential_storage")
            raw = stream.read(16385)
        if len(raw) > 16384:
            raise ValueError()
        value = json.loads(raw)
        if not isinstance(value, dict):
            raise ValueError()
        return value
    except (OSError, ValueError, UnicodeError):
        raise ConsumerError("credential_storage_unavailable") from None


def write_private(filename, value):
    temporary = None
    try:
        fd, temporary = tempfile.mkstemp(prefix=".plex-", dir=filename.parent)
        with os.fdopen(fd, "w") as stream:
            json.dump(value, stream)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, filename)
        temporary = None
        fd = os.open(filename.parent, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    except OSError:
        raise ConsumerError("credential_storage_unavailable") from None
    finally:
        if temporary:
            os.unlink(temporary)


def identity(folder):
    # Lock the private directory only for local identity publication, never network I/O.
    fd = os.open(folder, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        filename = folder / "client.json"
        if filename.exists():
            try:
                return text(read_private(filename)["client_id"], 128)
            except KeyError:
                raise ConsumerError("invalid_login_state") from None
        value = str(uuid.uuid4())
        write_private(filename, {"client_id": value})
        return value
    finally:
        os.close(fd)


def call(route, client_id, *, method="GET", params=None, secret=None):
    headers = {
        "Accept": "application/json",
        "X-Plex-Product": "Catabolic",
        "X-Plex-Client-Identifier": client_id,
    }
    if secret:
        headers["X-Plex-Token"] = secret
    query = urlencode(params or {})
    try:
        raw = request(
            "https://plex.tv/api/v2/" + route + ("?" + query if query else ""),
            method=method,
            headers=headers,
            timeout=15,
            maximum=65536,
            structured_errors=True,
        )
        result = json.loads(raw)
        if not isinstance(result, dict):
            raise ValueError()
        return result
    except ConsumerError:
        raise
    except (CatabolicError, ValueError, UnicodeError):
        raise ConsumerError("plex_login_failed") from None


def start(storage):
    folder = directory(storage)
    client_id = identity(folder)
    result = call("pins", client_id, method="POST", params={"strong": "true"})
    pin_id, code, ttl = result.get("id"), result.get("code"), result.get("expiresIn")
    if (
        type(pin_id) is not int
        or pin_id <= 0
        or type(ttl) is not int
        or not 0 < ttl <= 3600
    ):
        raise ConsumerError("invalid_response")
    text(code, 512)
    identifier = str(uuid.uuid4())
    write_private(
        folder / (identifier + ".json"),
        {
            "client_id": client_id,
            "pin_id": pin_id,
            "code": code,
            "expires_at": time.time() + ttl,
        },
    )
    return {
        "login_id": identifier,
        "authorization_url": "https://app.plex.tv/auth#?"
        + urlencode(
            {
                "clientID": client_id,
                "code": code,
                "context[device][product]": "Catabolic",
            }
        ),
        "expires_in": ttl,
        "state": "awaiting_authorization",
        "complete": False,
    }


def complete(storage, identifier):
    try:
        if str(uuid.UUID(identifier)) != identifier:
            raise ValueError()
    except (ValueError, TypeError, AttributeError):
        raise ConsumerError("invalid_login_id") from None
    folder = directory(storage)
    filename = folder / (identifier + ".json")
    session = read_private(filename)
    try:
        client_id = text(session["client_id"], 128)
        if "token" not in session:
            if (
                type(session["pin_id"]) is not int
                or session["pin_id"] <= 0
                or type(session["expires_at"]) not in (int, float)
            ):
                raise ConsumerError("invalid_login_state")
            text(session["code"], 512)
            if time.time() >= session["expires_at"]:
                return {
                    "state": "expired",
                    "complete": False,
                    "action": "start_new_login",
                }
            result = call(
                "pins/" + str(session["pin_id"]),
                client_id,
                params={"code": session["code"]},
            )
            if (
                result.get("id") != session["pin_id"]
                or result.get("code") != session["code"]
            ):
                raise ConsumerError("invalid_response")
            secret = result.get("authToken")
            if secret is None:
                return {
                    "state": "awaiting_authorization",
                    "complete": False,
                    "retry_after": 1,
                }
            text(secret, 4096)
            call("user", client_id, secret=secret)
            # Replace PIN with credential atomically; repeat completion never polls again.
            write_private(filename, {"client_id": client_id, "token": secret})
        else:
            text(session["token"], 4096)
    except (KeyError, TypeError):
        raise ConsumerError("invalid_login_state") from None
    return {"state": "authorized", "credential_file": str(filename), "complete": True}


def credential(reference):
    value = read_private(reference[5:])
    try:
        return text(value["token"], 4096), text(value["client_id"], 128)
    except KeyError:
        raise ConsumerError("invalid_login_state") from None
