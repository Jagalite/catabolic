"""Local administration on the caller's persistent native Store."""

import json

from .store import CatabolicError


class AccessError(CatabolicError):
    def __init__(self, code="forbidden", status=403):
        self.code, self.status = code, status
        super().__init__(code)


def _administrate(store, operation, request):
    result = json.loads(store._owner.access_admin(operation, request))
    if "error" in result:
        error = result["error"]
        raise AccessError(error["code"], error["status"])
    return result["value"]


def principal_put(store, profile, identifier):
    return _administrate(
        store, "principal_put", dict(profile=profile, identifier=identifier)
    )


def grant_put(store, principal, definition, identifier=None):
    result = _administrate(
        store,
        "grant_put",
        dict(principal=principal, definition=definition, identifier=identifier),
    )
    result["definition"] = definition
    return result


def issue(store, principal, grants, ttl=3600):
    return _administrate(
        store, "issue", dict(principal=principal, grants=grants, ttl=ttl)
    )
