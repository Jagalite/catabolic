# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Native v1 production owner, paired with frozen Python and numeric loopback only.

Each engine has an isolated database and independent in-memory receiver state.
The server URL is shared deliberately so reviewed plan hashes are comparable.
No live consumer, external credentials, media, or managed output is used.
"""

import json
import os
import re
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import threading
import time
import unittest
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlsplit
from xml.etree import ElementTree as ET

from tests.test_rust_legacy_review import BINARY, ORACLE, PYTHON, REFERENCE

SEED = """
from catabolic.store import Store
from catabolic.saved_queries import Queries
import json,sys
with Store(sys.argv[1],writable=True) as store:
    query=Queries(store).put('mapping-source',{'version':1,'mode':'rows','selection':{'language':'sql','query':sys.argv[4]}})['id']
    with store.transaction() as db:
        db.execute("INSERT INTO consumer_connections(id,profile,application,endpoint,credential_env,server_id,evidence) VALUES ('fixture','default',?,?,'MAPPING_FIXTURE_TOKEN','server-one','{}')",(sys.argv[2],sys.argv[3]))
print(query)
"""
QUERY = """
from catabolic.store import Store
from catabolic.saved_queries import Queries
import sys
with Store(sys.argv[1],writable=True) as store:
    print(Queries(store).put('mapping-source',{'version':1,'mode':'rows','selection':{'language':'sql','query':sys.argv[2]}})['id'])
"""


class Receiver:
    def __init__(self, application):
        self.application = application
        self.states = {engine: self.initial() for engine in (True, False)}
        self.engine = True
        self.requests = {engine: [] for engine in (True, False)}
        self.ignore = False
        self.fail_after_write = False
        self.after_write = None
        self.before_request = None
        self.pause_write = threading.Event()
        self.write_started = threading.Event()
        self.pause_write.set()
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                self.handle_request()

            def do_PUT(self):
                self.handle_request()

            def do_POST(self):
                self.handle_request()

            def do_DELETE(self):
                self.handle_request()

            def handle_request(self):
                parsed = urlsplit(self.path)
                params = {
                    k: v[0]
                    for k, v in parse_qs(parsed.query, keep_blank_values=True).items()
                }
                raw = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                body = json.loads(raw) if raw else None
                outer.requests[outer.engine].append(
                    (self.command, parsed.path, params, body)
                )
                try:
                    if outer.before_request:
                        outer.before_request(self.command, parsed.path)
                    status, payload = outer.respond(
                        self.command, parsed.path, params, body
                    )
                    if isinstance(payload, ET.Element):
                        payload = ET.tostring(payload)
                    elif not isinstance(payload, bytes):
                        payload = json.dumps(payload).encode()
                    self.send_response(status)
                    self.send_header("Content-Length", str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.endpoint = f"http://127.0.0.1:{self.server.server_port}"

    def close(self):
        self.pause_write.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    @property
    def key(self):
        return "1" if self.application == "plex" else "a" * 32

    @property
    def second_key(self):
        return "2" if self.application == "plex" else "c" * 32

    @property
    def collection(self):
        return "8" if self.application == "plex" else "b" * 32

    def initial(self):
        return {
            "title": "Remote title",
            "genres": ["Manual"],
            "tags": [],
            "identity": "guid-one",
            "members": [],
            "untouched": "Keep this",
        }

    def effect(self, update):
        self.write_started.set()
        self.pause_write.wait(10)
        if not self.ignore:
            update()
        if self.after_write:
            self.after_write()
        return (503 if self.fail_after_write else 200), b""

    def respond(self, method, route, params, body):
        state = self.states[self.engine]
        if self.application == "plex":
            if route == "/":
                return 200, ET.Element(
                    "MediaContainer", machineIdentifier="server-one", version="1.2.3"
                )
            if route == "/library/sections":
                root = ET.Element("MediaContainer")
                node = ET.SubElement(
                    root,
                    "Directory",
                    key="7",
                    uuid="library-one",
                    title="Movies",
                    type="movie",
                    createdAt="1",
                    refreshing="0",
                )
                ET.SubElement(node, "Location", path="/media")
                return 200, root
            if route == "/library/metadata/" + self.collection:
                root = ET.Element("MediaContainer")
                ET.SubElement(
                    root,
                    "Directory",
                    ratingKey=self.collection,
                    type="collection",
                    smart="0",
                    librarySectionID="7",
                    subtype="movie",
                    guid="collection-one",
                )
                return 200, root
            if route == "/library/metadata/" + self.collection + "/children":
                root = ET.Element(
                    "MediaContainer",
                    size=str(len(state["members"])),
                    totalSize=str(len(state["members"])),
                )
                for key in state["members"]:
                    ET.SubElement(root, "Video", ratingKey=key)
                return 200, root
            if route.startswith("/library/collections/"):

                def update():
                    if method == "PUT":
                        state["members"] = [self.key]
                    else:
                        state["members"] = []

                return self.effect(update)
            if route in (
                "/library/metadata/" + self.key,
                "/library/metadata/" + self.second_key,
            ):
                root = ET.Element("MediaContainer")
                node = ET.SubElement(
                    root,
                    "Video",
                    ratingKey=route.rsplit("/", 1)[-1],
                    type="movie",
                    librarySectionID="7",
                    guid=state["identity"],
                    title=state["title"],
                )
                media = ET.SubElement(node, "Media")
                ET.SubElement(media, "Part", file="/media/movie.mkv")
                ET.SubElement(node, "Field", name="title", locked="1")
                for tag in state["genres"]:
                    ET.SubElement(node, "Genre", tag=tag)
                for tag in state["tags"]:
                    ET.SubElement(node, "Label", tag=tag)
                return 200, root
            if route == "/library/sections/7/all" and method == "PUT":

                def update():
                    if "title.value" in params:
                        state["title"] = params["title.value"]
                    for field, key in [("genre", "genres"), ("label", "tags")]:
                        wanted = [
                            v
                            for k, v in params.items()
                            if re.fullmatch(field + r"\[\d+\]\.tag\.tag", k)
                        ]
                        removed = [
                            unquote(v)
                            for v in params.get(field + "[].tag.tag-", "").split(",")
                            if v
                        ]
                        state[key] = sorted(
                            (set(state[key]) | set(wanted)) - set(removed)
                        )

                return self.effect(update)
        else:
            if route == "/System/Info":
                return 200, {"Id": "server-one", "Version": "1.2.3"}
            if route == "/Library/VirtualFolders":
                return 200, [
                    {
                        "ItemId": "7",
                        "Name": "Movies",
                        "CollectionType": "movies",
                        "Locations": ["/media"],
                    }
                ]
            if route == "/Items" and params.get("Ids") == self.collection:
                return 200, {
                    "TotalRecordCount": 1,
                    "Items": [
                        {
                            "Id": self.collection,
                            "Type": "BoxSet",
                            "DateCreated": "2020",
                            "Path": "/collections/one",
                        }
                    ],
                }
            if route == "/Items" and params.get("ParentId") == self.collection:
                return 200, {
                    "TotalRecordCount": len(state["members"]),
                    "Items": [{"Id": key} for key in state["members"]],
                }
            if route == "/Collections/" + self.collection + "/Items":

                def update():
                    state["members"] = [self.key] if method == "POST" else []

                return self.effect(update)
            if route == "/Items" and params.get("Ids") in (self.key, self.second_key):
                return 200, {
                    "TotalRecordCount": 1,
                    "Items": [
                        {
                            "Id": params["Ids"],
                            "Type": "Movie",
                            "Path": "/media/movie.mkv",
                            "Name": state["title"],
                            "Genres": state["genres"],
                            "Tags": state["tags"],
                            "ProviderIds": {"fixture": state["identity"]},
                            "LockData": False,
                            "LockedFields": ["Name"],
                            "Overview": state["untouched"],
                        }
                    ],
                }
            if route in (
                "/Items/" + self.key + "/Ancestors",
                "/Items/" + self.second_key + "/Ancestors",
            ):
                return 200, [{"Id": "7"}]
            if route == "/Items/" + self.key and method == "POST":

                def update():
                    state["title"], state["genres"], state["tags"] = (
                        body["Name"],
                        body["Genres"],
                        body["Tags"],
                    )
                    state["untouched"] = body["Overview"]

                return self.effect(update)
        return 404, b""


class NativeDestinationMappingTest(unittest.TestCase):
    application = "plex"

    def setUp(self):
        for path in (PYTHON, REFERENCE / "catabolic/cli.py", BINARY):
            self.assertTrue(path.is_file(), str(path))
        self.root = Path(tempfile.mkdtemp(prefix="native-destination-"))
        self.addCleanup(shutil.rmtree, self.root)
        self.receiver = Receiver(self.application)
        self.addCleanup(self.receiver.close)
        self.db = {
            e: self.root / ("python.db" if e else "native.db") for e in (True, False)
        }
        self.env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith("CATABOLIC_") and k not in ("PYTHONPATH", "PYTHONHOME")
        }
        self.env.update(
            HOME=str(self.root),
            MAPPING_FIXTURE_TOKEN="disposable-loopback-token",
            PYTHONHASHSEED="0",
            PYTHONPATH=str(REFERENCE),
            TERM="dumb",
            COLUMNS="80",
            NO_COLOR="1",
        )
        # The frozen transport spawns `python -m catabolic.http_worker`.
        # Bind that child as well as the parent CLI to the verified oracle tree.
        origin = subprocess.run(
            [str(PYTHON), "-c", "import catabolic.http_worker as w; print(w.__file__)"],
            env=self.env,
            capture_output=True,
            text=True,
            timeout=15,
        )
        self.assertEqual(origin.returncode, 0, origin.stderr)
        self.assertEqual(
            Path(origin.stdout.strip()).resolve(),
            (REFERENCE / "catabolic/http_worker.py").resolve(),
        )
        result = self.call(True, ["init"], mode=[])
        self.assertEqual(result.returncode, 0, result.stderr)
        sql = f"SELECT 'one' k,'{self.receiver.key}' remote,'/media/movie.mkv' path,'Catalog title' title"
        query = self.python(
            SEED, str(self.db[True]), self.application, self.receiver.endpoint, sql
        ).strip()
        shutil.copyfile(self.db[True], self.db[False])
        self.definition = {
            "version": 1,
            "id": "fixture-map",
            "query": query,
            "destination": {"connection": "fixture", "library": "7"},
            "key": {"column": "k"},
            "target": {"id": {"column": "remote"}, "path": {"column": "path"}},
            "fields": {"title": {"column": "title"}},
        }
        self.definition_path = self.root / "definition.json"

    def python(self, program, *args):
        result = subprocess.run(
            [
                str(PYTHON),
                "-c",
                "import sys;sys.path.insert(0,sys.argv.pop(1));" + program,
                str(REFERENCE),
                *args,
            ],
            env=self.env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def command(self, engine, args, mode=None):
        mode = ["--machine"] if mode is None else mode
        return (
            [str(PYTHON), "-c", ORACLE, str(REFERENCE)] if engine else [str(BINARY)]
        ) + ["--db", str(self.db[engine]), *mode, *args]

    def call(self, engine, args, mode=None):
        self.receiver.engine = engine
        return subprocess.run(
            self.command(engine, args, mode),
            env=self.env,
            capture_output=True,
            text=True,
            timeout=35,
        )

    def mapping(
        self,
        engine,
        operation="mapping-preview",
        expected=None,
        maximum=None,
        mode=None,
    ):
        self.definition_path.write_text(json.dumps(self.definition, sort_keys=True))
        args = ["projection", operation, "--definition", str(self.definition_path)]
        if expected is not None:
            args += ["--expected-plan", expected]
        if maximum is not None:
            args += ["--max-removals", str(maximum)]
        return self.call(engine, args, mode)

    def data(self, result, code=0):
        self.assertEqual(result.returncode, code, result.stdout + result.stderr)
        return json.loads(result.stdout)["data"]

    def assert_typed(self, left, right):
        self.assertEqual(
            json.dumps(left, sort_keys=True, ensure_ascii=False),
            json.dumps(right, sort_keys=True, ensure_ascii=False),
        )

    def pair_preview(self):
        results = {e: self.mapping(e) for e in (True, False)}
        left, right = (self.data(results[e]) for e in (True, False))
        self.assert_typed(left, right)
        return left

    def pair_error(self, fragment, **kwargs):
        results = [self.mapping(e, **kwargs) for e in (True, False)]
        for result in results:
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertIn(fragment, result.stdout + result.stderr)
        self.assertEqual(json.loads(results[0].stdout), json.loads(results[1].stdout))

    def apply_pair(self, maximum=0, code=0):
        plan = self.pair_preview()
        result = {
            e: self.data(
                self.mapping(e, "mapping-apply", plan["plan_id"], maximum), code
            )
            for e in (True, False)
        }
        for value in result.values():
            for row in value["results"]:
                row["event_id"] = "<event>"
            if "event_id" in value:
                value["event_id"] = "<event>"
        self.assert_typed(result[True], result[False])
        self.assert_typed(self.receiver.states[True], self.receiver.states[False])
        self.assert_typed(
            [r for r in self.receiver.requests[True] if r[0] != "GET"],
            [r for r in self.receiver.requests[False] if r[0] != "GET"],
        )
        return result[False]

    def events(self, engine):
        with closing(sqlite3.connect(self.db[engine])) as db:
            return [
                (r[0], json.loads(r[1]), r[2])
                for r in db.execute(
                    "SELECT id,subject,severity FROM consumer_events WHERE event='mapping_publish' ORDER BY rowid"
                )
            ]

    def test_standard_preview_stdout_and_typed_dto_reports(self):
        for mode in ([], ["--json"], ["--machine"]):
            results = [self.mapping(e, mode=mode) for e in (True, False)]
            for result in results:
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            if self.application == "plex":
                self.assertEqual(results[0].stdout, results[1].stdout)
                self.assertEqual(results[0].stderr, results[1].stderr)
            elif mode:
                # Editable DTO order originates in a frozen Python set; only
                # typed report parity is claimed for this branch.
                self.assert_typed(
                    json.loads(results[0].stdout), json.loads(results[1].stdout)
                )

    def test_scalar_plan_apply_noop_and_ownership(self):
        before = {e: self.db[e].read_bytes() for e in self.db}
        plan = self.pair_preview()
        self.assertFalse(plan["applied"])
        for e in self.db:
            self.assertEqual(self.db[e].read_bytes(), before[e])
        self.assertTrue(self.apply_pair()["applied"])
        histories = {e: self.events(e) for e in self.db}
        self.assert_typed(histories[True][0][1:], histories[False][0][1:])
        count = {
            e: len([r for r in self.receiver.requests[e] if r[0] != "GET"])
            for e in self.db
        }
        self.apply_pair()
        for e in self.db:
            self.assertEqual(
                count[e], len([r for r in self.receiver.requests[e] if r[0] != "GET"])
            )
            self.receiver.states[e]["title"] = "Manual edit"
        self.pair_error("owned_value_changed_remotely")

    def test_owned_sets_and_removal_budget(self):
        self.definition["fields"] = {
            "genres": {"constant": ["Action", "Drama", "Manual"]}
        }
        self.apply_pair()
        for state in self.receiver.states.values():
            state["genres"].append("Later manual")
        self.definition["fields"] = {"genres": {"constant": []}}
        plan = self.pair_preview()
        self.assertEqual(plan["removals"], 2)
        self.pair_error(
            "removal_budget_exceeded",
            operation="mapping-apply",
            expected=plan["plan_id"],
        )
        self.apply_pair(maximum=2)
        self.assertEqual(
            self.receiver.states[False]["genres"], ["Later manual", "Manual"]
        )

    def test_stale_plan_validation_and_identity(self):
        plan = self.pair_preview()
        for state in self.receiver.states.values():
            state["title"] = "Changed remotely"
        self.pair_error(
            "stale_plan_preview_again",
            operation="mapping-apply",
            expected=plan["plan_id"],
        )
        self.pair_error(
            "apply_requires_expected_plan", operation="mapping-apply", expected=""
        )
        self.pair_error(
            "invalid_removal_budget",
            operation="mapping-apply",
            expected=plan["plan_id"],
            maximum=-1,
        )
        # The frozen CLI accepts arbitrary-size positive Python integers.
        self.assertTrue(self.apply_pair(maximum=10**100)["applied"])
        self.definition["fields"] = {"unsupported": {"constant": "x"}}
        self.pair_error("unsupported_fields")
        self.definition["fields"] = {"title": {"constant": ""}}
        self.pair_error("metadata_invalid_title")

    def test_uncertain_and_observation_only_recovery(self):
        self.receiver.ignore = True
        result = self.apply_pair(code=3)
        self.assertEqual(result["state"], "uncertain")
        for e in self.db:
            identifier = self.events(e)[0][0]
            before = self.db[e].read_bytes()
            preview = self.data(
                self.call(e, ["projection", "mapping-recover", identifier])
            )
            self.assertEqual(preview["state"], "not_applied")
            self.assertEqual(self.db[e].read_bytes(), before)
            recorded = self.data(
                self.call(e, ["projection", "mapping-recover", identifier, "--apply"])
            )
            self.assertTrue(recorded["safe_to_retry"])
            self.assertEqual(self.events(e)[0][1]["state"], "not_applied")
        self.receiver.ignore = False
        self.assertTrue(self.apply_pair()["applied"])

    def test_lost_response_is_not_retried_and_recovery_verifies(self):
        self.receiver.fail_after_write = True
        self.apply_pair(code=3)
        for e in self.db:
            mutations = [r for r in self.receiver.requests[e] if r[0] != "GET"]
            self.assertEqual(len(mutations), 1)
            identifier = self.events(e)[0][0]
            recovered = self.data(
                self.call(e, ["projection", "mapping-recover", identifier, "--apply"])
            )
            self.assertEqual(recovered["state"], "verified")
            self.assertEqual(
                len([r for r in self.receiver.requests[e] if r[0] != "GET"]), 1
            )

    def test_recovery_uncertain_and_unsupported_options(self):
        self.receiver.fail_after_write = True
        self.apply_pair(code=3)
        for e in self.db:
            self.receiver.states[e]["title"] = "Later edit"
            identifier = self.events(e)[0][0]
            recovered = self.data(
                self.call(e, ["projection", "mapping-recover", identifier, "--apply"]),
                3,
            )
            self.assertEqual(recovered["state"], "uncertain")
            bad = self.call(
                e,
                [
                    "projection",
                    "mapping-recover",
                    identifier,
                    "--expected-plan",
                    "anything",
                ],
            )
            self.assertEqual(bad.returncode, 2)
            self.assertIn("mapping_unsupported_recovery_options", bad.stdout)
            blocked = self.mapping(e)
            self.assertEqual(blocked.returncode, 2)
            self.assertIn("mapping_unresolved_write_recover_", blocked.stdout)
            self.assertEqual(
                json.loads(blocked.stdout)["data"]["error"]["type"], "ConsumerError"
            )

    def test_explicit_collections_and_missing_rows_retain(self):
        self.definition["destination"]["collections"] = [self.receiver.collection]
        self.definition["fields"] = {
            "collection_ids": {"constant": [self.receiver.collection]}
        }
        self.assertTrue(self.apply_pair()["applied"])
        self.assertEqual(self.receiver.states[False]["members"], [self.receiver.key])
        self.definition["fields"]["collection_ids"] = {"constant": []}
        self.apply_pair(maximum=1)
        self.assertEqual(self.receiver.states[False]["members"], [])

    def test_no_catalog_or_sqlite_lock_during_network(self):
        observed = []

        def check(method, route):
            path = self.db[self.receiver.engine]
            with closing(sqlite3.connect(path, timeout=0)) as db:
                db.execute("BEGIN IMMEDIATE")
                db.rollback()
            import fcntl

            with open(str(path) + ".lock", "a") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                fcntl.flock(lock, fcntl.LOCK_UN)
            observed.append((self.receiver.engine, method, route))

        self.receiver.before_request = check
        self.apply_pair()
        self.assertTrue(any(e and m != "GET" for e, m, _ in observed))
        self.assertTrue(any(not e and m != "GET" for e, m, _ in observed))

    def query(self, sql):
        # Saved revisions deliberately use random durable IDs. Seed the exact
        # same immutable fixture row into each otherwise independent catalog.
        identifier = self.python(QUERY, str(self.db[True]), sql).strip()
        with closing(sqlite3.connect(self.db[True])) as source:
            row = source.execute(
                "SELECT * FROM saved_queries WHERE id=?", (identifier,)
            ).fetchone()
        with closing(sqlite3.connect(self.db[False])) as target:
            target.execute(
                "INSERT OR IGNORE INTO saved_queries VALUES ("
                + ",".join("?" for _ in row)
                + ")",
                row,
            )
            target.commit()
        self.definition["query"] = identifier

    def test_query_grouping_expressions_and_complete_empty_results(self):
        key = self.receiver.key
        base = f"SELECT 'one' k,'{key}' remote,'/media/movie.mkv' path,'A' title"
        self.query(base + f" UNION ALL SELECT 'two','{key}','/media/movie.mkv','A'")
        self.pair_error("duplicate_target")
        self.query(base + f" UNION ALL SELECT 'one','{key}','/media/movie.mkv','B'")
        self.pair_error("conflicting_scalar_rows")
        self.query(base)
        self.definition["fields"] = {
            "genres": {"constant": '["A", "A", "B"]', "json": True}
        }
        self.assertEqual(
            self.pair_preview()["rows"][0]["after"]["genres"], ["A", "B", "Manual"]
        )
        self.definition["fields"] = {
            "genres": {"constant": "missing", "lookup": {"present": "A"}}
        }
        self.pair_error("unmapped_lookup_value")
        self.definition["fields"] = {"genres": {"constant": "bad json", "json": True}}
        self.pair_error("invalid_json_value")
        self.definition["fields"] = {"genres": {"constant": list(map(str, range(101)))}}
        self.pair_error("invalid_set")
        self.definition["fields"] = {"title": {"column": "title"}}
        self.query(base + " WHERE 0")
        self.assertEqual(self.pair_preview()["rows"], [])
        self.assertTrue(self.apply_pair()["applied"])
        for e in self.db:
            self.assertEqual(self.events(e), [])

    def test_truncated_query_and_duplicate_columns_fail_closed(self):
        key = self.receiver.key
        rows = " CROSS JOIN ".join(
            "(" + " UNION ALL ".join("SELECT 1" for _ in range(11)) + ")"
            for _ in range(3)
        )
        self.query(
            f"SELECT 'one' k,'{key}' remote,'/media/movie.mkv' path,'A' title FROM "
            + rows
        )
        self.pair_error("incomplete_query")
        self.query(
            f"SELECT 'one' k,'{key}' remote,'/media/movie.mkv' path,'A' title,'B' title"
        )
        self.pair_error("duplicate_columns")
        for e in self.db:
            self.assertFalse(self.events(e))

    def test_other_owner_remote_identity_and_owned_value_loss(self):
        self.apply_pair()
        self.definition["id"] = "other-owner"
        self.pair_error("field_owned_by_other_mapping")
        self.definition["id"] = "fixture-map"
        for state in self.receiver.states.values():
            state["identity"] = "replaced"
        self.pair_error("owned_target_identity_changed")

    def test_metadata_lock_contention_precedes_remote_and_releases(self):
        import fcntl

        for e in (True, False):
            plan = self.data(self.mapping(e))
            before = len(self.receiver.requests[e])
            with open(str(self.db[e]) + ".plex-metadata.lock", "a") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                result = self.mapping(e, "mapping-apply", plan["plan_id"])
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                self.assertIn("metadata_writer_busy_preview_again", result.stdout)
                self.assertEqual(before, len(self.receiver.requests[e]))
            self.data(self.mapping(e, "mapping-apply", plan["plan_id"]))

    def changed_batch(self, change_query):
        sql = f"SELECT 'one' k,'{self.receiver.key}' remote,'/media/movie.mkv' path,'First' title UNION ALL SELECT 'two','{self.receiver.second_key}','/media/movie.mkv','Second'"
        self.query(sql)
        plan = self.pair_preview()

        def change():
            with closing(sqlite3.connect(self.db[self.receiver.engine])) as db:
                if change_query:
                    row = db.execute(
                        "SELECT definition FROM saved_queries WHERE id=?",
                        (self.definition["query"],),
                    ).fetchone()
                    value = json.loads(row[0])
                    value["selection"]["query"] = sql + " WHERE 0"
                    db.execute(
                        "UPDATE saved_queries SET definition=? WHERE id=?",
                        (json.dumps(value, sort_keys=True), self.definition["query"]),
                    )
                else:
                    db.execute(
                        "UPDATE consumer_connections SET revision=revision+1 WHERE id='fixture'"
                    )
                db.commit()

        self.receiver.after_write = change
        expected = (
            "mapping_query_changed_preview_again"
            if change_query
            else "mapping_connection_changed"
        )
        errors = []
        for e in (True, False):
            result = self.mapping(e, "mapping-apply", plan["plan_id"])
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            errors.append(json.loads(result.stdout)["data"]["error"])
            self.assertEqual(errors[-1]["code"], expected)
            events = self.events(e)
            self.assertEqual(len(events), 1)
            self.assertEqual(events[0][1]["state"], "verified")
            self.assertEqual(
                len([r for r in self.receiver.requests[e] if r[0] != "GET"]), 1
            )
        self.assert_typed(*errors)
        self.assert_typed(self.events(True)[0][1:], self.events(False)[0][1:])

    def test_connection_revision_change_stops_after_durable_first_item(self):
        self.changed_batch(False)

    def test_query_change_stops_after_durable_first_item(self):
        self.changed_batch(True)

    def test_recovery_catalog_writer_contention_preserves_event_and_lock(self):
        import fcntl

        self.receiver.ignore = True
        self.apply_pair(code=3)
        errors = []
        for e in (True, False):
            identifier = self.events(e)[0][0]
            before = self.events(e)
            with open(str(self.db[e]) + ".lock", "a") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                result = self.call(
                    e, ["projection", "mapping-recover", identifier, "--apply"]
                )
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                errors.append(json.loads(result.stdout)["data"]["error"])
                self.assertEqual(
                    errors[-1]["type"],
                    "CatalogBusy",
                    "recovery contention diagnostic: "
                    + json.dumps(
                        {
                            "engine": "frozen_python" if e else "native",
                            "stdout": result.stdout,
                            "stderr": result.stderr,
                            "receiver_requests": self.receiver.requests[e],
                        },
                        ensure_ascii=False,
                        sort_keys=True,
                    ),
                )
                self.assertEqual(self.events(e), before)
                with open(str(self.db[e]) + ".plex-metadata.lock", "a") as publication:
                    fcntl.flock(publication, fcntl.LOCK_EX | fcntl.LOCK_NB)
            recovered = self.data(
                self.call(e, ["projection", "mapping-recover", identifier, "--apply"])
            )
            self.assertEqual(recovered["state"], "not_applied")
        self.assert_typed(*errors)

    def test_real_interrupt_after_write_keeps_started_recoverable(self):
        for e in (True, False):
            plan = self.data(self.mapping(e))
            self.receiver.engine = e
            self.receiver.write_started.clear()
            self.receiver.pause_write.clear()
            args = [
                "projection",
                "mapping-apply",
                "--definition",
                str(self.definition_path),
                "--expected-plan",
                plan["plan_id"],
            ]
            child = subprocess.Popen(
                self.command(e, args),
                env=self.env,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                self.assertTrue(self.receiver.write_started.wait(15))
                child.send_signal(signal.SIGINT)
                time.sleep(0.1)
                self.receiver.pause_write.set()
                out, err = child.communicate(timeout=20)
                self.assertEqual(child.returncode, 130, out + err)
            finally:
                self.receiver.pause_write.set()
                if child.poll() is None:
                    child.kill()
                    child.communicate()
            events = self.events(e)
            self.assertEqual(events[0][1]["state"], "started")
            result = self.data(
                self.call(e, ["projection", "mapping-recover", events[0][0], "--apply"])
            )
            self.assertEqual(result["state"], "verified")


class NativeJellyfinDestinationMappingTest(NativeDestinationMappingTest):
    application = "jellyfin"

    def numeric_fixture(self):
        self.rating = {True: 7, False: 7}
        self.reads = {True: 0, False: 0}
        self.switch_on_second = False
        original = self.receiver.respond

        def respond(method, route, params, body):
            status, payload = original(method, route, params, body)
            if (
                method == "GET"
                and route == "/Items"
                and params.get("Ids") == self.receiver.key
            ):
                engine = self.receiver.engine
                self.reads[engine] += 1
                if self.switch_on_second and self.reads[engine] >= 2:
                    self.rating[engine] = 7.0
                payload["Items"][0]["CommunityRating"] = self.rating[engine]
            return status, payload

        self.receiver.respond = respond

    def test_numeric_dto_prewrite_fence_matches_frozen(self):
        self.numeric_fixture()
        plan = self.pair_preview()
        self.reads = {True: 0, False: 0}
        self.switch_on_second = True
        result = {}
        for e in (True, False):
            result[e] = self.data(self.mapping(e, "mapping-apply", plan["plan_id"]))
            self.assertTrue(result[e]["applied"])
            self.assertEqual(
                len([r for r in self.receiver.requests[e] if r[0] != "GET"]), 1
            )
            self.assertEqual(self.events(e)[0][1]["state"], "verified")
            result[e]["results"][0]["event_id"] = "<event>"
        self.assert_typed(result[True], result[False])

    def test_numeric_dto_not_applied_recovery_matches_frozen(self):
        self.numeric_fixture()
        self.receiver.ignore = True
        self.apply_pair(code=3)
        self.rating = {True: 7.0, False: 7.0}
        results = []
        for e in (True, False):
            event = self.events(e)[0][0]
            result = self.data(
                self.call(e, ["projection", "mapping-recover", event, "--apply"])
            )
            self.assertEqual(result["state"], "not_applied")
            self.assertTrue(result["safe_to_retry"])
            self.assertEqual(self.events(e)[0][1]["state"], "not_applied")
            self.assertEqual(
                len([r for r in self.receiver.requests[e] if r[0] != "GET"]), 1
            )
            result["event_id"] = "<event>"
            results.append(result)
        self.assert_typed(*results)


if __name__ == "__main__":
    unittest.main()
