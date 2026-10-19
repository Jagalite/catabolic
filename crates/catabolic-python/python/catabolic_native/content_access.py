"""Pure revision lookup on the caller's persistent native Store."""

from .access import AccessError
from .store import CatabolicError


def revision_of(store, profile, file_id):
    try:
        return store._call(
            "content_access", "revision_of", {"file_id": file_id}, profile
        )
    except CatabolicError as exc:
        if str(exc) == "not_found":
            raise AccessError("not_found", 404) from None
        raise
