# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Small explicit HTTP adapters; credentials never enter catalog records."""

import base64
import json
import math
import os
import re
import sys
import urllib.parse
from datetime import datetime, timezone

from .curation import Curation, bounded_text, page_limit
from .domain import CatabolicError
from .process_runner import CommandFailure, command_output
from .store import encode


def request(url, *, headers=None, method="GET", maximum=1024 * 1024, timeout=15):
    """Bound the whole request, including DNS, TLS, headers and a slow body."""
    if (
        type(timeout) not in (int, float)
        or not math.isfinite(timeout)
        or not 0 < timeout <= 60
    ):
        raise CatabolicError(
            "HTTP timeout must be greater than zero and at most 60 seconds"
        )
    if type(maximum) is not int or not 0 <= maximum <= 8 * 1024 * 1024:
        raise CatabolicError("HTTP response limit must be 0..8 MiB")
    parsed = urllib.parse.urlsplit(url)
    if (
        parsed.scheme not in ("http", "https")
        or not parsed.hostname
        or parsed.username
        or parsed.password
    ):
        raise CatabolicError(
            "HTTP endpoint must use HTTP(S) without embedded credentials"
        )
    payload = encode(
        {
            "url": url,
            "headers": headers or {},
            "method": method,
            "maximum": maximum,
            "timeout": timeout,
        }
    ).encode()
    if len(payload) > 256 * 1024:
        raise CatabolicError("HTTP request exceeds input limit")
    try:
        raw = command_output(
            [sys.executable, "-m", "catabolic.http_worker"],
            input_bytes=payload,
            timeout=timeout,
            maximum=2 * maximum + 4096,
        )
        result = json.loads(raw)
        if "error" in result:
            raise CatabolicError(result["error"])
        return base64.b64decode(result["body"], validate=True)
    except CommandFailure as exc:
        if exc.state == "timeout":
            raise CatabolicError(
                "HTTP request exceeded the elapsed-time limit"
            ) from None
        raise CatabolicError("HTTP worker failed; check endpoint and network") from None
    except (OSError, ValueError, KeyError, TypeError):
        raise CatabolicError("HTTP worker failed; check endpoint and network") from None


def token(environment):
    if not isinstance(environment, str) or not re.fullmatch(
        r"[A-Za-z_][A-Za-z0-9_]*", environment
    ):
        raise CatabolicError("credential_env must be an environment variable name")
    value = os.environ.get(environment)
    if not value:
        raise CatabolicError(f"set credential environment variable {environment}")
    if "\n" in value or "\r" in value:
        raise CatabolicError("invalid credential value")
    return value


def tmdb_candidates(
    app, file_id, query, *, year=None, apply=False, credential_env="TMDB_TOKEN"
):
    bounded_text(query, "query", 512)
    if year is not None and (type(year) is not int or not 1 <= year <= 9999):
        raise CatabolicError("year must be 1..9999")
    params = {"query": query, "include_adult": "false", "page": 1}
    if year is not None:
        params["year"] = str(year)
    endpoint = "https://api.themoviedb.org/3/search/movie"
    response = json.loads(
        request(
            endpoint + "?" + urllib.parse.urlencode(params),
            headers={
                "Authorization": "Bearer " + token(credential_env),
                "Accept": "application/json",
            },
        )
    )
    if not isinstance(response, dict) or not isinstance(response.get("results"), list):
        raise CatabolicError("invalid TMDB response")
    candidates = []
    terms = set(re.findall(r"\w+", query.casefold()))
    fetched = datetime.now(timezone.utc).isoformat()
    for result in response["results"][:20]:
        if (
            not isinstance(result, dict)
            or type(result.get("id")) is not int
            or not isinstance(result.get("title"), str)
        ):
            continue
        metadata = {"title": result["title"]}
        released = result.get("release_date", "")
        if isinstance(released, str) and re.match(r"^\d{4}-", released):
            metadata["year"] = int(released[:4])
        found = set(re.findall(r"\w+", metadata["title"].casefold()))
        score = len(terms & found) / len(terms | found) if terms | found else 0
        evidence = {
            "provider": "tmdb.movie",
            "url": endpoint,
            "retrieved_at": fetched,
            "query": query,
            "title_word_overlap": score,
            "year_matches": metadata.get("year") == year if year else None,
            "ranking_method": "title token overlap; not a probability",
        }
        payload = {
            "item": {
                "kind": "movie",
                "identities": {"tmdb.movie": str(result["id"])},
                "metadata": metadata,
            }
        }
        candidate = {"file_id": file_id, "payload": payload, "evidence": evidence}
        if apply:
            candidate["proposal"] = Curation(app).put(
                file_id, payload, source="tmdb.movie", evidence=evidence
            )["id"]
        candidates.append(candidate)
    candidates.sort(key=lambda c: -c["evidence"]["title_word_overlap"])
    return {
        "candidates": candidates,
        "complete": response.get("total_pages", 1) <= 1,
        "provider_page": 1,
        "attribution": "This product uses the TMDB API but is not endorsed or certified by TMDB.",
    }


def record_output_change(db, profile, catalog):
    """Called in the same transaction as ownership publication, including recovery."""
    target = db.execute(
        "SELECT * FROM refresh_targets WHERE profile=? AND catalog=?",
        (profile, catalog),
    ).fetchone()
    if target:
        db.execute(
            "INSERT INTO refresh_dirty VALUES (?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET target=excluded.target",
            (profile, catalog, encode(dict(target))),
        )


class Refresh:
    def __init__(self, app):
        self.app, self.store, self.profile = app, app.store, app.profile

    def configure(self, catalog, endpoint, credential_env, application="jellyfin"):
        if application != "jellyfin":
            raise CatabolicError("the first refresh adapter supports jellyfin")
        parsed = urllib.parse.urlsplit(endpoint)
        if (
            parsed.scheme not in ("http", "https")
            or not parsed.hostname
            or parsed.username
            or parsed.password
            or parsed.query
            or parsed.fragment
        ):
            raise CatabolicError(
                "endpoint must be an HTTP(S) server base URL without credentials, query or fragment"
            )
        bounded_text(endpoint, "endpoint")
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", credential_env):
            raise CatabolicError("invalid credential environment variable name")
        if not self.store.rows("SELECT id FROM catalogs WHERE id=?", (catalog,)):
            raise CatabolicError("unknown catalog")
        with self.store.transaction() as db:
            db.execute(
                "INSERT INTO refresh_targets VALUES (?,?,?,?,?) ON CONFLICT(profile,catalog) DO UPDATE SET application=excluded.application,endpoint=excluded.endpoint,credential_env=excluded.credential_env",
                (
                    self.profile,
                    catalog,
                    application,
                    endpoint.rstrip("/"),
                    credential_env,
                ),
            )
        return {
            "catalog": catalog,
            "application": application,
            "endpoint": endpoint.rstrip("/"),
            "credential_env": credential_env,
        }

    def after_sync(self, result, catalog="global"):
        if not result.get("healthy"):
            return []
        ids = []
        with self.store.transaction() as db:
            dirty = list(
                db.execute(
                    "SELECT * FROM refresh_dirty WHERE profile=? AND (? IS NULL OR catalog=?)",
                    (self.profile, catalog, catalog),
                )
            )
            for row in dirty:
                cursor = db.execute(
                    "INSERT INTO refresh_events(profile,catalog,target) VALUES (?,?,?)",
                    (self.profile, row["catalog"], row["target"]),
                )
                ids.append(cursor.lastrowid)
                db.execute(
                    "DELETE FROM refresh_dirty WHERE profile=? AND catalog=?",
                    (self.profile, row["catalog"]),
                )
        return ids

    def list(self, *, limit=100, after=0):
        page_limit(limit)
        rows = self.store.rows(
            "SELECT * FROM refresh_events WHERE profile=? AND id>? ORDER BY id LIMIT ?",
            (self.profile, after, limit + 1),
        )
        more = len(rows) > limit
        rows = rows[:limit]
        for row in rows:
            row["target"] = json.loads(row["target"])
        return {"events": rows, "next_after": rows[-1]["id"] if more else None}

    def run(self, *, limit=100):
        page_limit(limit)
        rows = self.store.rows(
            "SELECT * FROM refresh_events WHERE profile=? AND state!='complete' AND attempts<3 ORDER BY id LIMIT ?",
            (self.profile, limit),
        )
        results = []
        for row in rows:
            target = json.loads(row["target"])
            with self.store.transaction() as db:
                db.execute(
                    "UPDATE refresh_events SET attempts=attempts+1 WHERE id=?",
                    (row["id"],),
                )
            try:
                request(
                    target["endpoint"] + "/Library/Refresh",
                    method="POST",
                    headers={"X-Emby-Token": token(target["credential_env"])},
                )
                state, error = "complete", None
            except CatabolicError as exc:
                state, error = "failed", str(exc)
            with self.store.transaction() as db:
                db.execute(
                    "UPDATE refresh_events SET state=?,error=?,finished_at=CURRENT_TIMESTAMP WHERE id=?",
                    (state, error, row["id"]),
                )
            results.append({"id": row["id"], "state": state, "error": error})
        return {
            "events": results,
            "complete": all(r["state"] == "complete" for r in results),
        }

    def retry(self, identifier):
        with self.store.transaction() as db:
            count = db.execute(
                "UPDATE refresh_events SET attempts=0,state='queued',error=NULL WHERE id=? AND profile=? AND state!='complete'",
                (identifier, self.profile),
            ).rowcount
        if not count:
            raise CatabolicError("unknown or completed refresh event")
        return {"queued": identifier}
