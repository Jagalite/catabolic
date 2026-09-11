# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Local token administration and shared resource/field authorization."""

import hashlib
import hmac
import json
import secrets
import time
from functools import cached_property
from uuid import uuid4

from ..domain import CatabolicError
from ..saved_queries import Queries
from ..store import encode


class AccessError(CatabolicError):
    def __init__(self, code="forbidden", status=403):
        self.code, self.status = code, status
        super().__init__(code)


def secret_pair():
    identifier = secrets.token_hex(12)
    secret = identifier + "." + secrets.token_urlsafe(32)
    return identifier, secret, hashlib.sha256(secret.encode()).hexdigest()


def principal_put(store, profile, identifier):
    if not isinstance(identifier, str) or not 1 <= len(identifier) <= 128:
        raise AccessError("invalid_principal", 400)
    with store.transaction() as db:
        db.execute(
            "INSERT INTO api_principals(id,profile) VALUES (?,?) ON CONFLICT(id) DO NOTHING",
            (identifier, profile),
        )
        if (
            db.execute(
                "SELECT profile FROM api_principals WHERE id=?", (identifier,)
            ).fetchone()[0]
            != profile
        ):
            raise AccessError("principal_profile_conflict", 409)
    return {"principal": identifier, "profile": profile}


def grant_put(store, principal, definition, identifier=None):
    fields = {
        "actions",
        "item_ids",
        "file_ids",
        "occurrence_ids",
        "query_id",
        "metadata_fields",
        "derivatives",
        "operation_ids",
        "projection_ids",
        "report_ids",
        "fallback_policy_ids",
        "operator",
        "revisions",
    }
    if not isinstance(definition, dict) or set(definition) - fields:
        raise AccessError("invalid_grant_fields", 400)
    for key in (
        "actions",
        "item_ids",
        "file_ids",
        "occurrence_ids",
        "metadata_fields",
        "operation_ids",
        "projection_ids",
        "report_ids",
        "fallback_policy_ids",
    ):
        value = definition.get(key, [])
        if (
            not isinstance(value, list)
            or len(value) > 10000
            or any(not isinstance(v, str) or len(v) > 256 for v in value)
        ):
            raise AccessError("invalid_grant", 400)
    if not definition.get("actions") or any(
        type(definition.get(k, False)) is not bool for k in ("operator", "derivatives")
    ):
        raise AccessError("invalid_grant", 400)
    if "revisions" in definition and (
        not isinstance(definition["revisions"], dict)
        or any(
            not isinstance(k, str) or not isinstance(v, str)
            for k, v in definition["revisions"].items()
        )
    ):
        raise AccessError("invalid_revisions", 400)
    row = store.rows("SELECT * FROM api_principals WHERE id=?", (principal,))
    if not row:
        raise AccessError("unknown_principal", 404)
    if definition.get("query_id"):
        entity, _, report = Queries(store, row[0]["profile"]).select(
            definition["query_id"], _http=True
        )
        if entity != "item_id" or not report["complete"]:
            raise AccessError("grant_requires_complete_item_selection", 400)
    identifier = identifier or str(uuid4())
    with store.transaction() as db:
        old = db.execute(
            "SELECT principal FROM api_grants WHERE id=?", (identifier,)
        ).fetchone()
        if old and old[0] != principal:
            raise AccessError("grant_principal_conflict", 409)
        db.execute(
            "INSERT INTO api_grants(id,principal,definition) VALUES (?,?,?) ON CONFLICT(id) DO UPDATE SET definition=excluded.definition,revision=revision+1,enabled=1",
            (identifier, principal, encode(definition)),
        )
        db.execute(
            "INSERT INTO api_audit(principal,action,resource) VALUES (?,'grant.put',?)",
            (principal, identifier),
        )
    return {"grant_id": identifier, "definition": definition}


def issue(store, principal, grants, ttl=3600):
    if (
        not isinstance(grants, list)
        or not grants
        or len(grants) > 100
        or type(ttl) is not int
        or not 1 <= ttl <= 2592000
    ):
        raise AccessError("invalid_token_configuration", 400)
    for identifier in grants:
        if not store.rows(
            "SELECT 1 FROM api_grants WHERE id=? AND principal=? AND enabled=1",
            (identifier, principal),
        ):
            raise AccessError("invalid_token_grant", 400)
    identifier, secret, digest = secret_pair()
    expires = time.time() + ttl
    with store.transaction() as db:
        db.execute(
            "INSERT INTO api_tokens(id,principal,digest,grants,expires) VALUES (?,?,?,?,?)",
            (identifier, principal, digest, encode(grants), expires),
        )
        db.execute(
            "INSERT INTO api_audit(principal,action,resource) VALUES (?,'token.issue',?)",
            (principal, identifier),
        )
    return {"token_id": identifier, "token": secret, "expires": expires}


def revoke(store, identifier):
    with store.transaction() as db:
        changed = db.execute(
            "UPDATE api_tokens SET revoked=1 WHERE id=?", (identifier,)
        ).rowcount
        db.execute(
            "INSERT INTO api_audit(action,resource) VALUES ('token.revoke',?)",
            (identifier,),
        )
    return {"revoked": bool(changed)}


def authenticate(store, secret=None, *, token_id=None):
    if token_id is None:
        if not isinstance(secret, str) or not 1 <= len(secret) <= 256:
            raise AccessError("unauthorized", 401)
        token_id = secret.split(".", 1)[0]
    rows = store.rows(
        "SELECT t.*,p.profile,p.enabled FROM api_tokens t JOIN api_principals p ON p.id=t.principal WHERE t.id=?",
        (token_id,),
    )
    if (
        not rows
        or rows[0]["revoked"]
        or not rows[0]["enabled"]
        or rows[0]["expires"] <= time.time()
    ):
        raise AccessError("unauthorized", 401)
    row = rows[0]
    if secret is not None and not hmac.compare_digest(
        row["digest"], hashlib.sha256(secret.encode()).hexdigest()
    ):
        raise AccessError("unauthorized", 401)
    return Access(store, row)


class Access:
    def __init__(self, store, token):
        self.store, self.token = store, token
        self.profile, self.principal = token["profile"], token["principal"]
        ids = json.loads(token["grants"])
        self.grants = []
        for identifier in ids:
            rows = store.rows(
                "SELECT * FROM api_grants WHERE id=? AND principal=? AND enabled=1",
                (identifier, self.principal),
            )
            if rows:
                self.grants.append(
                    {
                        **json.loads(rows[0]["definition"]),
                        "_id": identifier,
                        "_revision": rows[0]["revision"],
                    }
                )
        self._members = {}

    def matching(self, action):
        return [g for g in self.grants if action in g["actions"] or "*" in g["actions"]]

    def require(self, action):
        if not self.matching(action):
            raise AccessError()

    def operator(self, action):
        return any(g.get("operator") for g in self.matching(action))

    def members(self, grant):
        if grant["_id"] not in self._members:
            ids = set(grant.get("item_ids", []))
            if grant.get("query_id"):
                entity, selected, report = Queries(self.store, self.profile).select(
                    grant["query_id"], _http=True
                )
                if entity != "item_id" or not report["complete"]:
                    raise AccessError("incomplete_authorization_selection")
                ids.update(selected)
            self._members[grant["_id"]] = ids
        return self._members[grant["_id"]]

    def item(self, identifier, action="metadata:read"):
        return any(
            g.get("operator") or identifier in self.members(g)
            for g in self.matching(action)
        )

    def component(self, identifier):
        return self.operator("metadata:read") or any(
            identifier in g.get("occurrence_ids", [])
            for g in self.matching("component:read")
        )

    def file(self, identifier, action="metadata:read", revision=None):
        for g in self.matching(action):
            pinned = g.get("revisions", {}).get(identifier)
            if pinned is not None:
                from ..content_access import revision_of

                actual = (
                    revision
                    if revision is not None
                    else revision_of(self.store, self.profile, identifier)
                )
                if pinned != actual:
                    continue
            if g.get("operator") or identifier in g.get("file_ids", []):
                return True
            if g.get("derivatives"):
                rows = self.store.rows(
                    "SELECT source_item_id FROM media_outputs WHERE file_id=? AND profile=?",
                    (identifier, self.profile),
                )
                if any(row["source_item_id"] in self.members(g) for row in rows):
                    return True
        return False

    def processing(self, body):
        return any(
            (
                g.get("operator")
                or (
                    body["item_id"] in self.members(g)
                    and body["operation_id"] in g.get("operation_ids", [])
                )
            )
            and g.get("revisions", {}).get(
                body["source_file_id"], body["source_revision"]
            )
            == body["source_revision"]
            for g in self.matching("processing:request")
        )

    def metadata(self, identifier, value):
        keys = set()
        for grant in self.matching("metadata:read"):
            if grant.get("operator"):
                return value
            if identifier in self.members(grant):
                keys.update(grant.get("metadata_fields", ["title", "year"]))
        return {key: value[key] for key in keys if key in value}

    @cached_property
    def fingerprint(self):
        return hashlib.sha256(
            encode([self.principal, self.token["id"], self.grants]).encode()
        ).hexdigest()
