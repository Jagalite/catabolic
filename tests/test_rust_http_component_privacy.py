# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native loopback component packages against frozen 1.7 HTTP privacy behavior.

Run with the frozen 453fca983222c6665a48775eef67c489c96b527e Python source
first on PYTHONPATH. Python builds disposable catalogs and serves only the
independent oracle copy; every candidate HTTP request uses the Rust listener.
"""

import json
import os
import shlex
import shutil
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch

from catabolic import components
from catabolic.access import grant_put, issue, principal_put
from catabolic.curation import occurrence
from catabolic.fallback_policies import Policies
from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests import test_component_packages as fixtures
from tests import test_rust_http_transport as transport

NATIVE = Path(
    os.environ.get("CATABOLIC_NATIVE_BINARY", "target/debug/catabolic-native")
).resolve()


class NativeHTTPComponentPrivacyTest(unittest.TestCase):
    setUp = fixtures.PackageTest.setUp
    file = fixtures.PackageTest.file
    probe = fixtures.PackageTest.probe
    rows = fixtures.PackageTest.rows
    claim = fixtures.PackageTest.claim
    video = fixtures.PackageTest.video
    start_native_server = transport.NativeHTTPTransportTest.start_native_server
    stop_server = transport.NativeHTTPTransportTest.stop_server

    def query(self, name, *, kind=None, file_id=None, graphql=False):
        if graphql:
            filters = ["current:true"]
            if kind:
                filters.append("kind:" + json.dumps(kind))
            if file_id:
                filters.append("file:" + json.dumps(file_id))
            if kind in ("audio", "subtitle"):
                filters.append('language:"en"')
            if kind == "subtitle":
                filters.append("forced:true")
            selection = {
                "language": "graphql",
                "query": "query($after:String){componentOccurrences("
                + ",".join(filters)
                + ",after:$after){nodes{id} pageInfo{hasNextPage endCursor}}}",
            }
        else:
            conditions, params = ["current=1"], {}
            for key, value in (("kind", kind), ("file_id", file_id)):
                if value:
                    conditions.append(key + "=:" + key)
                    params[key] = value
            if kind in ("audio", "subtitle"):
                conditions.append("language='en'")
            if kind == "subtitle":
                conditions.append("forced=1")
            selection = {
                "language": "sql",
                "query": "SELECT occurrence_id FROM catalog_component_occurrences WHERE "
                + " AND ".join(conditions),
                "params": params,
            }
        return Queries(self.store).put(
            name, {"entity": "occurrence_id", "selection": selection}
        )["id"]

    def policy(
        self,
        video,
        *,
        publication="container",
        graphql=False,
        subtitle_files=None,
        supported_codecs=None,
    ):
        if graphql:
            primary = self.query("primary", kind="video", file_id=video, graphql=True)
        else:
            primary = Queries(self.store).put(
                "primary",
                {
                    "selection": {
                        "language": "sql",
                        "query": "SELECT id AS file_id FROM files WHERE id=:id",
                        "params": {"id": video},
                    }
                },
            )["id"]
        audio = self.query("audio", kind="audio", file_id=video, graphql=graphql)
        subtitles = [
            {
                "name": f"subtitle-{index}",
                "query_id": self.query(
                    f"subtitle-{index}",
                    kind="subtitle",
                    file_id=file_id,
                    graphql=graphql,
                ),
            }
            for index, file_id in enumerate(subtitle_files or [None])
        ]
        package = {
            "publication": publication,
            "requirements": [
                {
                    "name": "audio",
                    "fallbacks": [{"name": "english", "query_id": audio}],
                },
                {"name": "subtitles", "fallbacks": subtitles},
            ],
        }
        if supported_codecs is not None:
            package["supported_codecs"] = supported_codecs
        return Policies(self.store).put(
            "http-package",
            {
                "fallbacks": [{"name": "primary", "query_id": primary}],
                "failback": {"mode": "immediate"},
                "package": package,
            },
        )["id"]

    def sidecar(self, video, name, *, dependency=False):
        file_id = self.file(name, "subtitle", {"language": "en", "forced": True})
        row = self.rows("file_id=? AND current=1", (file_id,))[0]
        revision = components.revision(occurrence(self.store, "default", video))
        claims = {}
        if dependency:
            supporting = self.file("PRIVATE-SUPPORT.sub", "custom:support")
            claims = {
                "dependencies_complete": True,
                "dependencies": [
                    {
                        "file_id": supporting,
                        "revision": components.revision(
                            occurrence(self.store, "default", supporting)
                        ),
                        "purpose": "subtitle_data",
                    }
                ],
            }
        self.claim(
            row,
            compatibility={
                "edition_id": self.item,
                "video_file_id": video,
                "video_revision": revision,
                "part": None,
                "timeline_id": "container:" + revision,
                "coverage": "full",
                "offset_seconds": 0.0,
                "synchronization": "declared",
            },
            **claims,
        )
        return file_id, row["occurrence_id"]

    def start_http(self, policy, files, *, denied=(), sql_operator=True, helper=None):
        from fastapi.testclient import TestClient

        from catabolic.http.app import create_app

        self.policy_id = policy
        principal_put(self.store, "default", "component-viewer")
        self.definition = {
            "actions": [
                "metadata:read",
                "component:read",
                "content:read",
                "processing:request",
            ],
            "item_ids": [self.item],
            "file_ids": files,
            "occurrence_ids": [
                r["occurrence_id"]
                for r in self.rows("current=1")
                if r["occurrence_id"] not in denied
            ],
            "fallback_policy_ids": [policy],
            "metadata_fields": ["title", "year"],
        }
        self.grant = grant_put(self.store, "component-viewer", self.definition)[
            "grant_id"
        ]
        grants = [self.grant]
        if sql_operator:
            # SQL query authority does not grant metadata, file or component
            # access. The package still needs the explicit occurrence grants.
            grants.append(
                grant_put(
                    self.store,
                    "component-viewer",
                    {
                        "actions": ["sql:read"],
                        "operator": True,
                    },
                )["grant_id"]
            )
        credential = issue(self.store, "component-viewer", grants)
        self.headers = {"Authorization": "Bearer " + credential["token"]}
        with self.store.transaction() as db:
            db.execute("INSERT INTO api_sources VALUES ('default','media')")
        self.store.close()
        self.oracle_database = self.root / "frozen-http.db"
        shutil.copyfile(self.database, self.oracle_database)
        self.oracle = TestClient(create_app(self.oracle_database, read_only=False))
        self.addCleanup(self.oracle.close)
        self.start_native_server(
            env={**os.environ, "CATABOLIC_NATIVE_BINARY": str(helper or NATIVE)}
        )

    def pair(self, method, path, **kwargs):
        kwargs.setdefault("headers", self.headers)
        expected = self.oracle.request(method, path, **kwargs)
        actual = self.client.request(method, path, **kwargs)
        self.assertEqual(
            actual.status_code, expected.status_code, actual.text + expected.text
        )
        if "json" in expected.headers.get("content-type", ""):
            left, right = actual.json(), expected.json()
            for value in (left, right):
                if "request_id" in value:
                    self.assertTrue(value.pop("request_id"))
            self.assertEqual(left, right)
        else:
            self.assertEqual(actual.content, expected.content)
        return actual

    def resolve(self):
        result = self.pair(
            "POST",
            f"/v1/items/{self.item}/resolve",
            json={"fallback_policy_id": self.policy_id},
        )
        self.assertEqual(result.status_code, 200, result.text)
        self.assertNotIn(str(self.source), result.text)
        self.assertNotIn("PRIVATE-", result.text)
        package = result.json()["evidence"]["component_package"]
        self.assertNotIn("queries", package)
        for entry in package["components"]:
            for field in ("snapshot", "technical", "provenance", "asserted"):
                self.assertNotIn(field, entry["occurrence"])
        for dependency in package["dependencies"]:
            self.assertNotIn("snapshot", dependency)
        return result.json()

    def update_grant(self, **changes):
        self.definition.update(changes)
        for path in (self.database, self.oracle_database):
            with Store(path, writable=True) as store:
                grant_put(store, "component-viewer", self.definition, self.grant)

    def mixed_package(self, video, *, graphql_first):
        sql = self.query("mixed-sql", kind="video", file_id=video)
        graphql = self.query("mixed-graphql", kind="video", file_id=video, graphql=True)
        mixed = Queries(self.store).put(
            "mixed-primary",
            {
                "entity": "occurrence_id",
                "combine": "union",
                "queries": [graphql, sql] if graphql_first else [sql, graphql],
            },
        )["id"]
        definition = Policies(self.store).get(self.policy(video))["definition"]
        definition["fallbacks"] = [{"name": "mixed", "query_id": mixed}]
        return Policies(self.store).put("mixed-package", definition)["id"]

    def test_graphql_then_sql_package_preserves_traversal_denial(self):
        video = self.video("PRIVATE-VIDEO.mkv", True)
        self.start_http(self.mixed_package(video, graphql_first=True), [video])
        self.update_grant(operator=True)
        decision = self.resolve()
        self.assertEqual(decision["state"], "blocked")
        self.assertEqual(
            decision["evidence"]["component_package"]["packaging"]["reason"],
            "candidate_query_failed",
        )

    def test_sql_then_graphql_package_keeps_scope_for_requirement_queries(self):
        video = self.video("PRIVATE-VIDEO.mkv", True)
        self.start_http(self.mixed_package(video, graphql_first=False), [video])
        self.update_grant(operator=True)
        decision = self.resolve()
        self.assertEqual(decision["state"], "blocked")
        self.assertEqual(
            decision["evidence"]["component_package"]["packaging"]["reason"],
            "component_package_blocked",
        )

    def graphql_requirements(self, video):
        definition = Policies(self.store).get(self.policy(video, graphql=True))[
            "definition"
        ]
        primary = Policies(self.store).get(self.policy(video))["definition"][
            "fallbacks"
        ]
        definition["fallbacks"] = primary
        return Policies(self.store).put("graphql-requirements", definition)["id"]

    def test_graphql_requirement_skips_scoped_stale_occurrences(self):
        video = self.video("PRIVATE-VIDEO.mkv", True)
        self.start_http(self.graphql_requirements(video), [video])
        decision = self.resolve()
        self.assertEqual(decision["state"], "unresolved")
        self.assertEqual(decision["evidence"]["component_package"]["components"], [])

    def test_metadata_operator_graphql_scope_is_released_by_actual_probe(self):
        video = self.video("PRIVATE-VIDEO.mkv", True)
        self.start_http(self.graphql_requirements(video), [video])
        self.update_grant(operator=True)
        decision = self.resolve()
        self.assertEqual(decision["state"], "resolved_preferred")
        self.assertEqual(
            len(decision["evidence"]["component_package"]["components"]), 3
        )

    def test_token_revocation_during_component_probe_preserves_epoch_fence(self):
        self.authority_during_probe(expire=False)

    def test_natural_token_expiry_during_component_probe_reauthenticates(self):
        self.authority_during_probe(expire=True)

    def authority_during_probe(self, *, expire):
        from catabolic import fallback_resolution

        video = self.video("PRIVATE-VIDEO.mkv", True)
        started, release = self.root / "probe-started", self.root / "probe-release"
        helper = self.root / "controlled-native-helper"
        helper.write_text(
            "#!/bin/sh\n: > " + shlex.quote(str(started)) + "\n"
            "while [ ! -f "
            + shlex.quote(str(release))
            + " ]; do /bin/sleep 0.01; done\n"
            "exec " + shlex.quote(str(NATIVE)) + ' "$@"\n'
        )
        helper.chmod(0o700)
        jobs = self.store.rows("SELECT * FROM processing_jobs ORDER BY id")
        definition = Policies(self.store).get(self.policy(video))["definition"]
        definition["budgets"]["probe_timeout_ms"] = 5000
        policy = Policies(self.store).put("revocation-probe", definition)["id"]
        self.start_http(policy, [video], helper=helper)
        route = f"/v1/items/{self.item}/resolve"
        payload = {"fallback_policy_id": self.policy_id}

        def revoke(path):
            from catabolic.database_io import CatalogBusy

            deadline = time.monotonic() + 2
            while True:
                try:
                    store = Store(path, writable=True)
                    break
                except CatalogBusy:
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(0.005)
            with store:
                with store.transaction() as db:
                    db.execute("UPDATE api_tokens SET revoked=1")

        def expire_soon(path):
            expires = time.time() + 0.8
            with Store(path, writable=True) as store:
                with store.transaction() as db:
                    db.execute("UPDATE api_tokens SET expires=?", (expires,))
            return expires

        probe = fallback_resolution.probe
        oracle_expires = expire_soon(self.oracle_database) if expire else None

        def change_before_probe(*args, **kwargs):
            if expire:
                time.sleep(max(0, oracle_expires - time.time()) + 0.03)
            else:
                revoke(self.oracle_database)
            return probe(*args, **kwargs)

        with patch.object(fallback_resolution, "probe", change_before_probe):
            expected = self.oracle.post(route, headers=self.headers, json=payload)
        native_expires = expire_soon(self.database) if expire else None
        with ThreadPoolExecutor(max_workers=1) as pool:
            future = pool.submit(
                self.client.post, route, headers=self.headers, json=payload
            )
            try:
                deadline = time.monotonic() + 5
                while (
                    not started.exists()
                    and not future.done()
                    and time.monotonic() < deadline
                ):
                    time.sleep(0.005)
                self.assertTrue(
                    started.exists(), "native probe did not reach detached helper"
                )
                if expire:
                    time.sleep(max(0, native_expires - time.time()) + 0.03)
                else:
                    revoke(self.database)
            finally:
                release.touch()
            actual = future.result(timeout=10)
        self.assertEqual(expected.status_code, 401 if expire else 409, expected.text)
        self.assertEqual(actual.status_code, expected.status_code, actual.text)
        actual_body, expected_body = actual.json(), expected.json()
        self.assertTrue(actual_body.pop("request_id"))
        self.assertTrue(expected_body.pop("request_id"))
        self.assertEqual(actual_body, expected_body)
        self.assertEqual(
            actual_body["code"], "unauthorized" if expire else "catalog_error"
        )
        for path in (self.database, self.oracle_database):
            with Store(path) as store:
                self.assertEqual(
                    store.rows("SELECT * FROM processing_jobs ORDER BY id"), jobs
                )
                self.assertEqual(
                    store.rows(
                        "SELECT * FROM fallback_probe_slots WHERE pid IS NOT NULL"
                    ),
                    [],
                )

    def test_repeated_package_graphql_scope_preserves_catalog_error(self):
        video = self.video("PRIVATE-VIDEO.mkv", True)
        jobs = self.store.rows("SELECT * FROM processing_jobs ORDER BY id")
        self.start_http(self.policy(video, graphql=True), [video])
        self.update_grant(operator=True)
        response = self.pair(
            "POST",
            f"/v1/items/{self.item}/resolve",
            json={"fallback_policy_id": self.policy_id},
        )
        self.assertEqual(response.status_code, 500)
        self.assertEqual(response.json()["code"], "catalog_error")
        for path in (self.database, self.oracle_database):
            with Store(path) as store:
                self.assertEqual(
                    store.rows("SELECT * FROM processing_jobs ORDER BY id"), jobs
                )

    def test_ordinary_policy_graphql_uses_approved_global_ids_before_content_filter(
        self,
    ):
        video = self.video("PRIVATE-VIDEO.mkv", True)
        query = Queries(self.store).put(
            "ordinary-path-selection",
            {
                "selection": {
                    "language": "graphql",
                    "query": 'query($after:String){files(search:"PRIVATE-VIDEO",after:$after){nodes{id} pageInfo{hasNextPage endCursor}}}',
                }
            },
        )["id"]
        policy = Policies(self.store).put(
            "ordinary-policy", {"fallbacks": [{"name": "video", "query_id": query}]}
        )["id"]
        self.start_http(policy, [video], sql_operator=False)
        listing = self.pair(
            "POST",
            "/v1/query/graphql",
            json={"query": '{files(search:"PRIVATE-VIDEO"){nodes{id}}}'},
        )
        self.assertEqual(listing.json()["data"]["files"]["nodes"], [])
        resolved = self.pair(
            "POST",
            f"/v1/items/{self.item}/resolve",
            json={"fallback_policy_id": policy},
        )
        self.assertEqual(resolved.status_code, 200, resolved.text)
        self.assertEqual(resolved.json()["file_id"], video)
        self.assertNotIn("PRIVATE-VIDEO", resolved.text)
        self.assertEqual(
            self.pair("GET", resolved.json()["content_path"]).content, b"fixture"
        )

    def test_ready_container_has_sanitized_exact_package_and_concrete_bytes(self):
        video = self.video("PRIVATE-VIDEO.mkv", True)
        self.start_http(self.policy(video), [video])
        decision = self.resolve()
        self.assertEqual(decision["file_id"], video)
        package = decision["evidence"]["component_package"]
        self.assertEqual(len(package["components"]), 3)
        self.assertTrue(package["packaging"]["ready"])
        self.assertEqual(package["packaging"]["action"], "use_container")
        content = self.pair("GET", decision["content_path"])
        self.assertEqual(content.status_code, 200)
        self.assertEqual(content.content, b"fixture")

    def test_occurrence_only_external_candidate_does_not_grant_file_or_bytes(self):
        video = self.video("PRIVATE-VIDEO.mkv")
        sidecar, selected = self.sidecar(video, "PRIVATE-SELECTED.srt")
        self.start_http(self.policy(video, publication="sidecars"), [video])
        decision = self.resolve()
        package = decision["evidence"]["component_package"]
        self.assertEqual(package["packaging"]["action"], "publish_sidecars")
        self.assertTrue(package["packaging"]["ready"])
        chosen = package["components"][-1]["occurrence"]
        self.assertEqual(chosen["occurrence_id"], selected)
        self.assertEqual(chosen["file_id"], sidecar)
        self.assertEqual(self.pair("GET", f"/v1/files/{sidecar}").status_code, 404)
        endpoint = f"/v1/files/{sidecar}/content?revision={chosen['revision']}"
        denied = self.pair("GET", endpoint)
        self.assertEqual(denied.status_code, 404)
        self.assertNotEqual(denied.content, b"fixture")
        ticket = self.pair(
            "POST",
            "/v1/content-access",
            json={
                "file_id": sidecar,
                "revision": chosen["revision"],
            },
        )
        self.assertEqual(ticket.status_code, 404)

    def test_graphql_component_tiers_fail_closed_and_hide_denied_occurrences(self):
        video = self.video("PRIVATE-VIDEO.mkv")
        hidden_file, hidden = self.sidecar(video, "PRIVATE-DENIED.srt")
        allowed_file, allowed = self.sidecar(video, "PRIVATE-ALLOWED.srt")
        policy = self.policy(
            video,
            publication="sidecars",
            graphql=True,
            subtitle_files=[hidden_file, allowed_file],
        )
        self.start_http(policy, [video], denied=[hidden])
        listing = self.pair(
            "POST",
            "/v1/query/graphql",
            json={
                "query": '{componentOccurrences(kind:"subtitle",current:true){nodes{id fileId kind language} pageInfo{hasNextPage}}}'
            },
        )
        self.assertNotIn("errors", listing.json(), listing.text)
        self.assertEqual(
            listing.json()["data"]["componentOccurrences"]["nodes"],
            [
                {
                    "id": allowed,
                    "fileId": allowed_file,
                    "kind": "subtitle",
                    "language": "en",
                }
            ],
        )
        self.assertNotIn(hidden, listing.text)
        self.assertNotIn(hidden_file, listing.text)
        decision = self.resolve()
        # Frozen authorized GraphQL views redact file location/path before
        # component revision validation. The native owner must preserve that
        # fail-closed result without returning raw occurrence evidence.
        self.assertEqual(decision["state"], "blocked")
        self.assertIsNone(decision["file_id"])
        self.assertIsNone(decision["content_path"])
        package = decision["evidence"]["component_package"]
        self.assertEqual(package["components"], [])
        self.assertEqual(package["packaging"]["reason"], "component_package_blocked")
        self.assertNotIn(hidden, json.dumps(decision))
        self.assertNotIn(hidden_file, json.dumps(decision))

    def test_graphql_private_component_fields_remain_forbidden(self):
        video = self.video("PRIVATE-VIDEO.mkv", True)
        self.start_http(self.policy(video), [video])
        for field in ("snapshot", "technical", "provenance", "asserted"):
            with self.subTest(field=field):
                result = self.pair(
                    "POST",
                    "/v1/query/graphql",
                    json={
                        "query": "{componentOccurrences{nodes{" + field + "}}}",
                    },
                )
                self.assertIn("errors", result.json())
                if field != "snapshot":
                    self.assertIn("FORBIDDEN", result.text)
                self.assertNotIn(str(self.source), result.text)
                self.assertNotIn("PRIVATE-VIDEO", result.text)

    def test_not_ready_selected_package_never_advertises_parent_content(self):
        video = self.video("PRIVATE-VIDEO.mkv", True)
        streams = [json.loads(r["technical"]) for r in self.rows("file_id=?", (video,))]
        streams.append(
            {
                "index": 3,
                "codec_type": "audio",
                "codec_name": "aac",
                "tags": {"language": "fra"},
            }
        )
        self.probe(video, streams)
        self.start_http(self.policy(video, publication="selected_only"), [video])
        decision = self.resolve()
        self.assertEqual(decision["file_id"], video)
        self.assertEqual(decision["state"], "resolved_preferred")
        self.assertIsNone(decision["content_path"])
        packaging = decision["evidence"]["component_package"]["packaging"]
        self.assertEqual(packaging["action"], "mux")
        self.assertFalse(packaging["ready"])
        self.assertNotIn("/content", json.dumps(decision))
        with Store(self.database) as store:
            self.assertEqual(
                store.rows("SELECT id FROM processing_jobs WHERE operation='render'"),
                [],
            )

    def test_conversion_package_also_has_no_content_path(self):
        video = self.video("PRIVATE-VIDEO.mkv", True)
        self.start_http(self.policy(video, supported_codecs=["h264"]), [video])
        decision = self.resolve()
        self.assertIsNone(decision["content_path"])
        packaging = decision["evidence"]["component_package"]["packaging"]
        self.assertEqual(packaging["action"], "convert")
        self.assertFalse(packaging["ready"])

    def test_authorized_dependency_snapshot_is_removed_from_public_evidence(self):
        video = self.video("PRIVATE-VIDEO.mkv")
        self.sidecar(video, "PRIVATE-SELECTED.srt", dependency=True)
        supporting = self.store.rows(
            "SELECT id FROM files WHERE path='PRIVATE-SUPPORT.sub'"
        )[0]["id"]
        self.start_http(self.policy(video, publication="sidecars"), [video, supporting])
        decision = self.resolve()
        dependencies = decision["evidence"]["component_package"]["dependencies"]
        self.assertEqual(len(dependencies), 1)
        self.assertEqual(dependencies[0]["file_id"], supporting)
        self.assertNotIn("snapshot", dependencies[0])

    def test_denied_required_component_returns_empty_blocked_package(self):
        video = self.video("PRIVATE-VIDEO.mkv", True)
        hidden = self.rows("file_id=? AND kind='subtitle'", (video,))[0][
            "occurrence_id"
        ]
        self.start_http(self.policy(video), [video], denied=[hidden])
        decision = self.resolve()
        self.assertIsNone(decision["file_id"])
        self.assertIsNone(decision["content_path"])
        self.assertEqual(decision["reasons"], ["no_eligible_content"])
        package = decision["evidence"]["component_package"]
        self.assertEqual(package["components"], [])
        self.assertEqual(package["dependencies"], [])
        self.assertEqual(package["packaging"]["action"], "block")
        self.assertFalse(package["packaging"]["ready"])
        self.assertNotIn(hidden, json.dumps(decision))

    def test_dependency_authority_is_not_implied_by_component_permission(self):
        video = self.video("PRIVATE-VIDEO.mkv")
        self.sidecar(video, "PRIVATE-SELECTED.srt", dependency=True)
        supporting = self.store.rows(
            "SELECT id FROM files WHERE path='PRIVATE-SUPPORT.sub'"
        )[0]["id"]
        self.start_http(self.policy(video, publication="sidecars"), [video])
        decision = self.resolve()
        self.assertIsNone(decision["file_id"])
        self.assertIsNone(decision["content_path"])
        self.assertEqual(decision["evidence"]["component_package"]["components"], [])
        self.assertNotIn(supporting, json.dumps(decision))

    def test_changed_component_source_is_not_reused_without_rescanning(self):
        video = self.video("PRIVATE-VIDEO.mkv")
        _, selected = self.sidecar(video, "PRIVATE-SELECTED.srt")
        self.start_http(self.policy(video, publication="sidecars"), [video])
        self.assertEqual(self.resolve()["file_id"], video)
        (self.source / "PRIVATE-SELECTED.srt").write_bytes(
            b"changed component revision"
        )
        decision = self.resolve()
        self.assertIsNone(decision["file_id"])
        self.assertIsNone(decision["content_path"])
        self.assertNotIn(selected, json.dumps(decision))

    def test_package_policy_does_not_grant_sql_query_authority(self):
        video = self.video("PRIVATE-VIDEO.mkv", True)
        self.start_http(self.policy(video), [video], sql_operator=False)
        decision = self.resolve()
        self.assertEqual(decision["state"], "blocked")
        self.assertIsNone(decision["file_id"])
        self.assertIsNone(decision["content_path"])
        package = decision["evidence"]["component_package"]
        self.assertEqual(package["components"], [])
        self.assertEqual(package["packaging"]["reason"], "candidate_query_failed")

    def test_component_revocation_is_rechecked_on_next_resolve_and_graphql(self):
        video = self.video("PRIVATE-VIDEO.mkv", True)
        hidden = self.rows("file_id=? AND kind='subtitle'", (video,))[0][
            "occurrence_id"
        ]
        self.start_http(self.policy(video), [video])
        self.assertEqual(self.resolve()["file_id"], video)
        self.update_grant(
            occurrence_ids=[v for v in self.definition["occurrence_ids"] if v != hidden]
        )
        decision = self.resolve()
        self.assertIsNone(decision["file_id"])
        self.assertNotIn(hidden, json.dumps(decision))
        result = self.pair(
            "POST",
            "/v1/query/graphql",
            json={
                "query": '{componentOccurrences(kind:"subtitle"){nodes{id} pageInfo{hasNextPage}}}'
            },
        )
        self.assertEqual(result.json()["data"]["componentOccurrences"]["nodes"], [])

    def test_logical_component_demand_fails_closed_without_admitting_work(self):
        video = self.video("PRIVATE-VIDEO.mkv", True)
        self.start_http(self.policy(video, publication="selected_only"), [video])
        with Store(self.database) as store:
            before = store.rows("SELECT id FROM processing_jobs ORDER BY id")
        body = {
            "item_id": self.item,
            "fallback_policy_id": self.policy_id,
            "operation_id": "explicit-component-operation",
        }
        for key in ("component-demand", "component-demand", "new-component-demand"):
            result = self.pair(
                "POST",
                "/v1/rendition-requests/logical",
                headers={**self.headers, "Idempotency-Key": key},
                json=body,
            )
            self.assertEqual(result.status_code, 409, result.text)
            self.assertEqual(
                result.json()["code"],
                "component_packaging_requires_explicit_local_admission",
            )
        with Store(self.database) as store:
            self.assertEqual(
                store.rows("SELECT id FROM processing_jobs ORDER BY id"), before
            )
            for table in (
                "api_logical_demands",
                "api_requests",
                "processing_artifacts",
            ):
                self.assertEqual(store.rows("SELECT * FROM " + table), [])


if __name__ == "__main__":
    unittest.main()
