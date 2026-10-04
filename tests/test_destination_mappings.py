# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import contextlib
import copy
import io
import json
import re
import unittest
from unittest.mock import patch
from urllib.parse import parse_qs, unquote, urlsplit
from xml.etree import ElementTree as ET

from catabolic.consumer_adapters import ConsumerError, Jellyfin
from catabolic.consumer_setup import put_connection
from catabolic.destination_mappings import events, recover, run
from catabolic.mapping_adapters import capabilities
from catabolic.saved_queries import Queries
from catabolic.store import Store
from tests import test_plex_metadata as plex_fixture


class MappingTest(unittest.TestCase):
    setUp = plex_fixture.PlexMetadataTest.setUp
    curate = plex_fixture.PlexMetadataTest.curate

    def protocol(self, route, method="GET", params=None):
        collections = getattr(self, "collections", {})
        parts = route.split("/")
        if len(parts) >= 4 and parts[3] in collections:
            key = parts[3]
            c = collections[key]
            if method in ("PUT", "DELETE"):
                self.assertEqual(parts[2], "collections")
                self.assertEqual(parts[4], "items")
                self.calls.append((route, method, params))
                if method == "PUT":
                    self.assertEqual(
                        params["uri"],
                        "server://server-one/com.plexapp.plugins.library/library/metadata/1",
                    )
                    c["members"].add("1")
                else:
                    c["members"].remove(parts[-1])
                return b""
            root = ET.Element("MediaContainer")
            if route.endswith("/children"):
                for member in sorted(c["members"]):
                    ET.SubElement(root, "Video", ratingKey=member)
                root.set("size", str(len(root)))
                root.set("totalSize", str(len(root) + (1 if c.get("truncated") else 0)))
            else:
                ET.SubElement(
                    root,
                    "Directory",
                    ratingKey=key,
                    type="collection",
                    smart=c.get("smart", "0"),
                    librarySectionID="7",
                    subtype="movie",
                    guid=c["guid"],
                )
            return ET.tostring(root)
        if method == "PUT" and route == "/library/sections/7/all":
            for field in ("genre", "label"):
                names = [
                    v
                    for k, v in params.items()
                    if re.fullmatch(field + r"\[\d+\]\.tag\.tag", k)
                ]
                removed = [
                    unquote(v)
                    for v in params.get(field + "[].tag.tag-", "").split(",")
                    if v
                ]
                if names or removed:
                    old = {n.get("tag") for n in self.node.findall(field.title())}
                    for n in list(self.node.findall(field.title())):
                        self.node.remove(n)
                    for name in sorted((old | set(names)) - set(removed)):
                        ET.SubElement(self.node, field.title(), tag=name)
        return plex_fixture.PlexMetadataTest.protocol(self, route, method, params)

    def definition(
        self,
        sql="SELECT 'one' AS k, '1' AS remote, '/media/movie.mkv' AS path, 'Catalog title' AS title",
        fields=None,
    ):
        with Store(self.database, writable=True) as store:
            query = Queries(store, "default").put(
                "mapping-source",
                {
                    "version": 1,
                    "mode": "rows",
                    "selection": {"language": "sql", "query": sql},
                },
            )
        return {
            "version": 1,
            "id": "curated",
            "query": query["id"],
            "destination": {"connection": "home", "library": "7"},
            "key": {"column": "k"},
            "target": {"id": {"column": "remote"}, "path": {"column": "path"}},
            "fields": fields or {"title": {"column": "title"}},
        }

    def apply(self, definition, **kw):
        plan = run(self.database, "default", definition)
        return run(
            self.database,
            "default",
            definition,
            apply=True,
            expected_plan=plan["plan_id"],
            **kw,
        )

    def test_scalar_preview_apply_noop_and_remote_conflict(self):
        d = self.definition()
        before = ET.tostring(self.node)
        p = run(self.database, "default", d)
        self.assertEqual(ET.tostring(self.node), before)
        self.assertFalse(events(self.database, "default"))
        result = run(
            self.database, "default", d, apply=True, expected_plan=p["plan_id"]
        )
        self.assertTrue(result["applied"])
        self.assertEqual(self.node.get("title"), "Catalog title")
        count = len([c for c in self.calls if c[1] == "PUT"])
        self.assertTrue(self.apply(d)["applied"])
        self.assertEqual(len([c for c in self.calls if c[1] == "PUT"]), count)
        self.node.set("title", "Manual edit")
        with self.assertRaisesRegex(ConsumerError, "owned_value_changed"):
            run(self.database, "default", d)

    def test_grouped_sets_preserve_manual_and_only_remove_owned(self):
        ET.SubElement(self.node, "Genre", tag="Manual")
        sql = "SELECT 'one' k,'1' remote,'/media/movie.mkv' path,'genre:a' value UNION ALL SELECT 'one','1','/media/movie.mkv','genre:b'"
        d = self.definition(
            sql,
            {
                "genres": {
                    "column": "value",
                    "lookup": {"genre:a": "Action", "genre:b": "Drama"},
                }
            },
        )
        p = run(self.database, "default", d)
        self.assertEqual(p["rows"][0]["after"]["genres"], ["Action", "Drama", "Manual"])
        self.assertTrue(self.apply(d)["applied"])
        ET.SubElement(self.node, "Genre", tag="Later manual")
        d["fields"] = {"genres": {"constant": []}}
        p = run(self.database, "default", d)
        self.assertEqual(p["removals"], 2)
        with self.assertRaisesRegex(ConsumerError, "removal_budget"):
            self.apply(d)
        self.assertTrue(self.apply(d, max_removals=2)["applied"])
        self.assertEqual(
            {n.get("tag") for n in self.node.findall("Genre")},
            {"Manual", "Later manual"},
        )

    def test_preexisting_tag_never_owned(self):
        ET.SubElement(self.node, "Genre", tag="Existing")
        d = self.definition(fields={"genres": {"constant": ["Existing"]}})
        self.assertTrue(self.apply(d)["applied"])
        d["fields"]["genres"]["constant"] = []
        self.assertEqual(run(self.database, "default", d)["removals"], 0)
        self.assertTrue(self.apply(d)["applied"])
        self.assertEqual(self.node.find("Genre").get("tag"), "Existing")

    def test_stale_plan_and_unsupported_fields_write_nothing(self):
        d = self.definition()
        p = run(self.database, "default", d)
        self.node.set("title", "Changed")
        with self.assertRaisesRegex(ConsumerError, "stale_plan"):
            run(self.database, "default", d, apply=True, expected_plan=p["plan_id"])
        d["fields"] = {"tags": {"constant": ["private"]}}
        with self.assertRaisesRegex(ConsumerError, "unsupported_fields"):
            run(self.database, "default", d)
        self.assertFalse([c for c in self.calls if c[1] == "PUT"])

    def test_duplicate_target_and_conflicting_scalar_rows(self):
        for sql, error in [
            (
                "SELECT 'one' k,'1' remote,'/media/movie.mkv' path,'A' title UNION ALL SELECT 'two','1','/media/movie.mkv','A'",
                "duplicate_target",
            ),
            (
                "SELECT 'one' k,'1' remote,'/media/movie.mkv' path,'A' title UNION ALL SELECT 'one','1','/media/movie.mkv','B'",
                "conflicting_scalar",
            ),
        ]:
            with (
                self.subTest(error=error),
                self.assertRaisesRegex(ConsumerError, error),
            ):
                run(self.database, "default", self.definition(sql))

    def test_truncated_and_empty_query(self):
        sql = (
            "SELECT 'one' k,'1' remote,'/media/movie.mkv' path,'title' title FROM "
            + " CROSS JOIN ".join(
                "(" + " UNION ALL ".join("SELECT 1" for _ in range(11)) + ")"
                for _ in range(3)
            )
        )
        with self.assertRaisesRegex(ConsumerError, "incomplete_query"):
            run(self.database, "default", self.definition(sql))
        d = self.definition()
        self.assertTrue(self.apply(d)["applied"])
        empty = self.definition(
            "SELECT 'one' k,'1' remote,'/media/movie.mkv' path,'title' title WHERE 0"
        )
        p = run(self.database, "default", empty)
        self.assertEqual(p["rows"], [])
        self.assertEqual(p["removals"], 0)

    def test_other_owner_and_identity_change(self):
        d = self.definition()
        self.apply(d)
        other = {**d, "id": "different-owner"}
        with self.assertRaisesRegex(ConsumerError, "owned_by_other"):
            run(self.database, "default", other)
        self.node.set("guid", "replaced")
        with self.assertRaisesRegex(ConsumerError, "identity_changed"):
            run(self.database, "default", d)

    def test_uncertain_response_recovery_and_fence(self):
        d = self.definition()
        self.after_put = lambda: (_ for _ in ()).throw(ConsumerError("lost_response"))
        r = self.apply(d)
        self.assertEqual(r["state"], "uncertain")
        with self.assertRaisesRegex(ConsumerError, "unresolved_write"):
            run(self.database, "default", d)
        self.assertEqual(
            recover(self.database, "default", r["event_id"])["state"], "verified"
        )
        self.assertEqual(events(self.database, "default")[-1]["state"], "uncertain")
        self.assertEqual(
            recover(self.database, "default", r["event_id"], apply=True)["state"],
            "verified",
        )
        self.after_put = None
        self.assertTrue(self.apply(d)["applied"])

    def test_failed_write_recovery_and_wrong_path(self):
        d = self.definition()
        self.put_error = ConsumerError("unavailable")
        r = self.apply(d)
        self.assertEqual(
            recover(self.database, "default", r["event_id"], apply=True)["state"],
            "not_applied",
        )
        d["target"]["path"] = {"constant": "/wrong.mkv"}
        with self.assertRaisesRegex(ConsumerError, "path_mismatch"):
            run(self.database, "default", d)

    def test_json_set_and_unmapped_lookup(self):
        d = self.definition(fields={"genres": {"constant": '["A", "B"]', "json": True}})
        self.assertTrue(self.apply(d)["applied"])
        d["fields"] = {"genres": {"constant": "unknown", "lookup": {"known": "A"}}}
        with self.assertRaisesRegex(ConsumerError, "unmapped_lookup"):
            run(self.database, "default", d)

    def test_evaluation_uses_destination_field_validation(self):
        from catabolic.destination_mappings import evaluate

        class OtherDestination:
            capabilities = {"priority": "scalar", "categories": "set"}

            @staticmethod
            def validate_value(field, value):
                if field != "priority" or type(value) is not int:
                    raise ValueError("priority must be an integer")
                return value

        d = self.definition(
            fields={"priority": {"constant": 5}, "categories": {"constant": ["A"]}}
        )
        output = evaluate(self.database, "default", d, OtherDestination())
        self.assertEqual(
            output["rows"][0]["values"], {"priority": 5, "categories": ["A"]}
        )

    def test_capabilities(self):
        self.assertIn("labels", capabilities()["plex"]["fields"])
        self.assertNotIn("labels", capabilities()["jellyfin"]["fields"])
        self.assertIn("tags", capabilities()["jellyfin"]["fields"])

    def test_collection_membership_adoption_and_owned_removal(self):
        self.collections = {
            "20": {"guid": "collection://20", "members": {"99"}},
            "21": {"guid": "collection://21", "members": {"1", "88"}},
        }
        d = self.definition(fields={"collection_ids": {"constant": ["20", "21"]}})
        d["destination"]["collections"] = ["20", "21"]
        self.assertTrue(self.apply(d)["applied"])
        self.assertEqual(self.collections["20"]["members"], {"1", "99"})
        d["fields"]["collection_ids"] = {"constant": []}
        self.assertTrue(self.apply(d, max_removals=1)["applied"])
        self.assertEqual(self.collections["20"]["members"], {"99"})
        self.assertEqual(self.collections["21"]["members"], {"1", "88"})

    def test_collection_smart_truncated_and_identity_guards(self):
        self.collections = {
            "20": {"guid": "collection://20", "members": set(), "smart": "1"}
        }
        d = self.definition(fields={"collection_ids": {"constant": ["20"]}})
        d["destination"]["collections"] = ["20"]
        with self.assertRaisesRegex(ConsumerError, "unsupported_collection"):
            run(self.database, "default", d)
        self.collections["20"]["smart"] = "0"
        self.collections["20"]["truncated"] = True
        with self.assertRaisesRegex(ConsumerError, "incomplete_collection"):
            run(self.database, "default", d)
        self.collections["20"]["truncated"] = False
        self.assertTrue(self.apply(d)["applied"])
        self.collections["20"]["guid"] = "collection://replacement"
        with self.assertRaisesRegex(ConsumerError, "identity_changed"):
            run(self.database, "default", d)

    def test_collection_scope_and_separate_mapping_required(self):
        self.collections = {"20": {"guid": "collection://20", "members": set()}}
        d = self.definition(fields={"collection_ids": {"constant": ["30"]}})
        d["destination"]["collections"] = ["20"]
        with self.assertRaisesRegex(ConsumerError, "outside_adopted"):
            run(self.database, "default", d)
        d["fields"]["title"] = {"constant": "No"}
        with self.assertRaisesRegex(ConsumerError, "separate_definition"):
            run(self.database, "default", d)

    def test_cli_preview_apply_and_event_listing(self):
        from catabolic.cli import main

        d = self.definition()
        definition_file = self.root / "mapping.json"
        definition_file.write_text(json.dumps(d))

        def cli(*args):
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                code = main(
                    ["--db", str(self.database), "--machine", "projection", *args]
                )
            self.assertEqual(code, 0, out.getvalue())
            return json.loads(out.getvalue())["data"]

        p = cli("mapping-preview", "--definition", str(definition_file))
        r = cli(
            "mapping-apply",
            "--definition",
            str(definition_file),
            "--expected-plan",
            p["plan_id"],
        )
        self.assertTrue(r["applied"])
        self.assertEqual(cli("mapping-events")["events"][-1]["state"], "verified")
        self.assertIn("jellyfin", cli("mapping-capabilities"))
        event = cli("mapping-events", "--event", r["results"][0]["event_id"])
        self.assertEqual(event["before"]["values"]["title"], "Plex title")
        self.assertEqual(event["after"]["title"], "Catalog title")

    def test_cli_uncertain_write_exits_incomplete(self):
        from catabolic.cli import main

        d = self.definition()
        file = self.root / "mapping.json"
        file.write_text(json.dumps(d))
        plan = run(self.database, "default", d)
        self.put_ignored = True
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = main(
                [
                    "--db",
                    str(self.database),
                    "--machine",
                    "projection",
                    "mapping-apply",
                    "--definition",
                    str(file),
                    "--expected-plan",
                    plan["plan_id"],
                ]
            )
        self.assertEqual(code, 3)
        response = json.loads(output.getvalue())
        self.assertEqual(response["outcome"], "incomplete")
        self.assertFalse(response["data"]["complete"])

    def test_interruption_is_journaled_and_recovery_does_not_overwrite(self):
        d = self.definition()

        def interrupt():
            raise KeyboardInterrupt()

        self.after_put = interrupt
        with self.assertRaises(KeyboardInterrupt):
            self.apply(d)
        e = events(self.database, "default")[-1]
        self.assertEqual(e["state"], "started")
        self.node.set("title", "Manual after interruption")
        self.assertEqual(
            recover(self.database, "default", e["id"], apply=True)["state"], "uncertain"
        )
        self.assertEqual(self.node.get("title"), "Manual after interruption")
        with self.assertRaisesRegex(ConsumerError, "unresolved_write"):
            run(self.database, "default", d)

    def test_plex_request_encodes_tags_without_touching_scalar_metadata(self):
        d = self.definition(fields={"genres": {"constant": ["A&B, C/+東京"]}})
        self.assertTrue(self.apply(d)["applied"])
        self.assertEqual(self.node.get("title"), "Plex title")
        d["fields"]["genres"] = {"constant": []}
        self.assertTrue(self.apply(d, max_removals=1)["applied"])
        request = [c for c in self.calls if c[1] == "PUT"][-1][2]
        self.assertEqual(
            request["genre[].tag.tag-"], "A%26B%2C%20C%2F%2B%E6%9D%B1%E4%BA%AC"
        )


class JellyfinMappingTest(unittest.TestCase):
    curate = MappingTest.curate
    protocol = MappingTest.protocol
    definition = MappingTest.definition
    apply = MappingTest.apply

    # Use the same local catalog fixture, with a separate remote JSON protocol.
    def setUp(self):
        MappingTest.setUp(self)
        self.jid = "a" * 32
        self.dto = {
            "Id": self.jid,
            "Type": "Movie",
            "Path": "/media/movie.mkv",
            "Name": "Original",
            "Genres": ["Manual"],
            "Tags": [],
            "ProviderIds": {"Tmdb": "42"},
            "LockData": False,
            "LockedFields": ["Overview"],
            "Overview": "Keep",
            "Studios": [{"Name": "Studio"}],
            "ProductionYear": 2024,
            "PremiereDate": "2024-01-01T00:00:00Z",
            "CommunityRating": 7.0,
        }
        self.jwrites = []
        self.jlost = False
        mock = patch.object(Jellyfin, "call", side_effect=self.jprotocol)
        mock.start()
        self.addCleanup(mock.stop)
        put_connection(
            self.database,
            "default",
            "jelly",
            "jellyfin",
            "http://jelly.test",
            "JELLY_TEST",
            apply=True,
        )

    def jprotocol(self, route, method="GET", params=None, *, body=None):
        with Store(self.database, writable=True):
            pass
        if route == "/System/Info":
            return json.dumps({"Id": "jelly-server", "Version": "10.11.8"}).encode()
        if route == "/Library/VirtualFolders":
            return json.dumps(
                [
                    {
                        "ItemId": "b" * 32,
                        "Name": "Movies",
                        "CollectionType": "movies",
                        "Locations": ["/media"],
                    }
                ]
            ).encode()
        if route == "/Items/" + self.jid and method == "POST":
            self.jwrites.append(json.loads(body))
            self.dto.update(json.loads(body))
            if self.jlost:
                raise ConsumerError("lost_response")
            return b""
        if route.startswith("/Collections/"):
            self.jcollections[route.split("/")[2]]["members"].discard(
                params["ids"]
            ) if method == "DELETE" else self.jcollections[route.split("/")[2]][
                "members"
            ].add(params["ids"])
            if self.jlost:
                raise ConsumerError("lost_response")
            return b""
        if route.startswith("/Items?"):
            q = parse_qs(urlsplit(route).query)
            collection = (q.get("ParentId") or q.get("Ids"))[0]
            if collection in getattr(self, "jcollections", {}):
                c = self.jcollections[collection]
                items = (
                    [{"Id": k} for k in sorted(c["members"])]
                    if "ParentId" in q
                    else [
                        {
                            "Id": collection,
                            "Type": "BoxSet",
                            "Path": "/collections/curated",
                            "DateCreated": "2026-01-01T00:00:00Z",
                        }
                    ]
                )
                return json.dumps(
                    {"Items": items, "TotalRecordCount": len(items)}
                ).encode()
            self.assertEqual(parse_qs(urlsplit(route).query)["Ids"], [self.jid])
            return json.dumps({"Items": [self.dto], "TotalRecordCount": 1}).encode()
        if route == "/Items/" + self.jid + "/Ancestors":
            return json.dumps([{"Id": "b" * 32}]).encode()
        self.fail("unexpected Jellyfin request: " + route)

    def jelly_definition(self, fields):
        d = self.definition(fields=fields)
        d["destination"] = {"connection": "jelly", "library": "b" * 32}
        d["target"]["id"] = {"constant": self.jid}
        return d

    def test_jellyfin_preserves_full_metadata_and_owned_sets(self):
        before = copy.deepcopy(self.dto)
        d = self.jelly_definition(
            {"title": {"constant": "New title"}, "genres": {"constant": ["Action"]}}
        )
        self.assertTrue(self.apply(d)["applied"])
        self.assertEqual(
            self.dto, {**before, "Name": "New title", "Genres": ["Action", "Manual"]}
        )
        d["fields"]["genres"] = {"constant": []}
        self.assertTrue(self.apply(d, max_removals=1)["applied"])
        self.assertEqual(self.dto["Genres"], ["Manual"])

    def test_jellyfin_lost_response_recovery(self):
        d = self.jelly_definition({"tags": {"constant": ["Curated"]}})
        self.jlost = True
        r = self.apply(d)
        self.assertEqual(r["state"], "uncertain")
        self.assertEqual(
            recover(self.database, "default", r["event_id"], apply=True)["state"],
            "verified",
        )

    def test_jellyfin_incomplete_payload_and_unsupported_kind(self):
        d = self.jelly_definition({"title": {"constant": "Title"}})
        del self.dto["ProviderIds"]
        with self.assertRaisesRegex(ConsumerError, "incomplete_metadata"):
            run(self.database, "default", d)
        self.dto["Type"] = "Series"
        with self.assertRaisesRegex(ConsumerError, "identity_mismatch"):
            run(self.database, "default", d)
        self.assertEqual(self.jwrites, [])

    def test_jellyfin_collection_recovery_and_preserve_manual(self):
        collection = "c" * 32
        self.jcollections = {collection: {"members": {"d" * 32}}}
        d = self.jelly_definition({"collection_ids": {"constant": [collection]}})
        d["destination"]["collections"] = [collection]
        self.jlost = True
        result = self.apply(d)
        self.assertEqual(result["state"], "uncertain")
        self.assertEqual(
            recover(self.database, "default", result["event_id"], apply=True)["state"],
            "verified",
        )
        self.jlost = False
        d["fields"]["collection_ids"] = {"constant": []}
        self.assertTrue(self.apply(d, max_removals=1)["applied"])
        self.assertEqual(self.jcollections[collection]["members"], {"d" * 32})
        self.assertEqual(self.jwrites, [])


class MappingTransportTest(unittest.TestCase):
    def test_jellyfin_json_body_and_auth_headers(self):
        connection = {
            "application": "jellyfin",
            "endpoint": "https://jelly.example",
            "credential_env": "JELLY_TOKEN",
        }
        with (
            patch("catabolic.consumer_adapters.token", return_value="fixture-secret"),
            patch("catabolic.consumer_adapters.request", return_value=b"") as request,
        ):
            Jellyfin(connection).call(
                "/Items/" + "a" * 32, "POST", body=b'{"Name":"Example"}'
            )
        args, kw = request.call_args
        self.assertNotIn("fixture-secret", args[0])
        self.assertEqual(kw["headers"]["X-Emby-Token"], "fixture-secret")
        self.assertEqual(kw["headers"]["Content-Type"], "application/json")
        self.assertEqual(kw["body"], b'{"Name":"Example"}')
        self.assertTrue(kw["structured_errors"])
