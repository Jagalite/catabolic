"""Revision-pinned components on the caller's persistent native Store."""

import json

from . import _native
from .store import CatabolicError, NativeCommandHandle


def revision(snapshot):
    return _native.component_revision(json.dumps(snapshot, allow_nan=False))


def index_file(store, profile, file_id, *, historical=False):
    return store._call(
        "components",
        "index_file",
        dict(file_id=file_id, historical=bool(historical)),
        profile,
    )


def backfill(db):
    if not isinstance(db, NativeCommandHandle):
        raise CatabolicError("backfill requires a native Store command handle")
    # Validate the exact token atomically with adoption, on the same owner.
    return db._store._call("components", "backfill", dict(token=db._token))


def get(store, profile, identifier, *, require_current=False, session=None):
    request = dict(id=identifier, require_current=bool(require_current))
    if session is not None:
        from .evaluation import EvaluationSession

        if not isinstance(session, EvaluationSession):
            raise CatabolicError(
                "session requires a same-owner catabolic_native.evaluation.EvaluationSession; "
                "foreign Python evaluation/access contexts are unsupported"
            )
        request.update(
            session=session._token,
            cancelled=bool(session.cancelled),
            session_owner_matches=session.store is store and session.profile == profile,
        )
    return store._call(
        "components",
        "get",
        request,
        profile,
        _cancelled=(lambda: session.cancelled) if session is not None else None,
    )
