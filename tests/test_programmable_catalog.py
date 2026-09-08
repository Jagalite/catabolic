# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

import contextlib
import copy
import io
import json
import unittest

from catabolic.cli import main
from catabolic.domain import CatabolicError
from catabolic.layouts import PRESETS
from catabolic.operations import Operations
from catabolic.processing import Processing
from catabolic.projections import Projections
from catabolic.rules import Rules
from catabolic.saved_queries import Queries
from tests import test_query_layouts as fixtures
from tests.test_query_layouts import GRAPHQL_TRACKS, sql_selection


class ProgrammableCatalogTest(unittest.TestCase):
    setUp = fixtures.QueryLayoutTest.setUp

    def save(
        self,
        name="tracks",
        sql="SELECT item_id FROM catalog_items WHERE kind='track'",
        **kwargs,
    ):
        return Queries(self.store).put(
            name, {"selection": sql_selection(sql), **kwargs}
        )

    def projection(self, query):
        self.layouts.put("flat", copy.deepcopy(PRESETS["flat"]))
        return Projections(self.app).put("query", query["id"], "flat")

    def test_immutable_revisions_and_composition(self):
        queries = Queries(self.store)
        first = self.save()
        self.assertEqual(first["id"], self.save()["id"])
        other = self.save(
            sql="SELECT item_id FROM catalog_items WHERE item_id='track1'"
        )
        self.assertEqual(other["revision"], 2)
        difference = queries.put(
            "remaining",
            {"combine": "difference", "queries": [first["id"], other["id"]]},
        )
        self.assertEqual(queries.run(difference["id"])["ids"], ["track2"])
        self.assertEqual(len(queries.run(first["id"])["ids"]), 2)
        self.assertEqual(len(self.store.rows("SELECT * FROM query_dependencies")), 2)

    def test_complete_selection_vs_details_and_language_parity(self):
        queries = Queries(self.store)
        sql = self.save()
        graphql = queries.put(
            "graphql",
            {
                "selection": {
                    "language": "graphql",
                    "query": GRAPHQL_TRACKS,
                    "variables": {"kind": "track"},
                }
            },
        )
        self.assertEqual(
            queries.run(sql["id"])["ids"], queries.run(graphql["id"])["ids"]
        )
        short = queries.run(sql["id"], limit=1)
        self.assertTrue(
            short["complete"]
            and short["details_truncated"]
            and short["selection_complete"]
        )
        limited = self.save("limited", max_ids=1)
        with self.assertRaisesRegex(CatabolicError, "budget"):
            queries.select(limited["id"])
        random = self.save(
            "random", "SELECT item_id FROM catalog_items WHERE random()>0"
        )
        with self.assertRaises(CatabolicError):
            queries.select(random["id"])

    def test_projection_preview_atomic_execute_repeat_and_empty_guard(self):
        query = self.save()
        self.projection(query)
        projection = Projections(self.app)
        before = fixtures.QueryLayoutTest.state(self)
        preview = projection.run("query")
        self.assertTrue(preview["safe"])
        self.assertFalse(preview["applied"])
        self.assertEqual(before, fixtures.QueryLayoutTest.state(self))
        self.assertEqual(list((self.root / "query").iterdir()), [])
        bounded = projection.run("query", limit=1)
        self.assertTrue(
            bounded["safe"] and bounded["reconciliation"]["actions_truncated"]
        )
        self.assertEqual(len(bounded["reconciliation"]["actions"]), 1)
        self.assertGreater(bounded["reconciliation"]["action_count"], 1)
        result = projection.run("query", apply=True)
        self.assertTrue(result["complete"])
        paths = {
            str(p): p.lstat().st_ino
            for p in (self.root / "query").rglob("*")
            if p.is_symlink()
        }
        self.assertEqual(len(paths), 2)
        repeat = projection.run("query", apply=True)
        self.assertEqual(repeat["layout"]["change_count"], 0)
        self.assertEqual(
            paths,
            {
                str(p): p.lstat().st_ino
                for p in (self.root / "query").rglob("*")
                if p.is_symlink()
            },
        )
        empty = self.save("empty", "SELECT item_id FROM catalog_items WHERE 0")
        projection.put("query", empty["id"], "flat")
        blocked = projection.run("query", apply=True)
        self.assertFalse(blocked["safe"])
        budget = projection.run("query", apply=True, allow_empty=True)
        self.assertFalse(budget["safe"])
        self.assertTrue(
            projection.run("query", apply=True, allow_empty=True, max_removals=2)[
                "complete"
            ]
        )

    def test_arbitrary_queries_are_inspectable_but_not_projection_selections(self):
        query = self.save("rows", "SELECT * FROM catalog_items", mode="rows")
        result = Queries(self.store).run(query["id"], limit=1)
        self.assertFalse(result["complete"])
        with self.assertRaisesRegex(CatabolicError, "complete ID selection"):
            self.projection(query)

    def test_machine_cli_preserves_payload_and_error_contract(self):
        query = self.save()
        with contextlib.redirect_stdout(io.StringIO()) as output:
            code = main(
                ["--db", str(self.path), "--machine", "query", "run", query["id"]]
            )
        result = json.loads(output.getvalue())
        self.assertEqual(
            (code, result["interface_version"], result["outcome"]), (0, 1, "success")
        )
        self.assertEqual(result["data"]["ids"], ["track1", "track2"])
        with contextlib.redirect_stdout(io.StringIO()) as output:
            code = main(
                ["--db", str(self.path), "--machine", "query", "show", "missing"]
            )
        self.assertEqual(code, 2)
        self.assertEqual(
            json.loads(output.getvalue())["data"]["error"]["type"], "CatabolicError"
        )

    def test_analysis_gap_convergence_dedup_and_version(self):
        operation = Operations(self.app).put(
            "checksum", {"kind": "analysis", "operation": "hash"}
        )
        query = self.save(
            "missing-checksum",
            "SELECT f.file_id FROM catalog_files f WHERE f.profile=:profile AND f.status='present' AND NOT EXISTS (SELECT 1 FROM catalog_facts x WHERE x.profile=f.profile AND x.file_id=f.file_id AND x.operation='hash' AND x.current=1)",
        )
        rules = Rules(self.app)
        rule = rules.put("hash", operation["id"], None, {"query_id": query["id"]})
        other = rules.put("same-hash", operation["id"], None, {"query_id": query["id"]})
        planned = rules.preview(rule["id"])
        self.assertGreater(planned["matched_inputs"], 0)
        applied = rules.apply(rule["id"], batch=1000)
        self.assertTrue(applied["safe"])
        self.assertEqual(rules.apply(other["id"], batch=1000)["queued"], [])
        result = rules.run(rule["id"], batch=1000)
        self.assertTrue(result["complete"], result)
        self.assertEqual(Queries(self.store).run(query["id"])["ids"], [])
        count = len(self.store.rows("SELECT * FROM processing_jobs"))
        self.assertEqual(rules.apply(rule["id"])["queued"], [])
        self.assertTrue(rules.run(rule["id"])["complete"])
        self.assertEqual(len(self.store.rows("SELECT * FROM processing_jobs")), count)
        self.assertIsNone(rules.statistics(rule["id"])["calibration"])
        changed = Operations(self.app).put(
            "checksum",
            {"kind": "analysis", "operation": "hash", "options": {"timeout": 300}},
        )
        self.assertEqual(changed["revision"], 2)
        all_files = self.save(
            "files", "SELECT file_id FROM catalog_files WHERE profile=:profile"
        )
        changed_rule = rules.put(
            "hash", changed["id"], None, {"query_id": all_files["id"]}
        )
        self.assertGreater(
            len(rules.apply(changed_rule["id"], batch=1000)["queued"]), 0
        )

    def test_analysis_failure_retry_and_changed_source(self):
        operation = Operations(self.app).put(
            "sniff", {"kind": "analysis", "operation": "sniff"}
        )
        selection = {
            "language": "sql",
            "query": "SELECT file_id FROM catalog_files WHERE source_relative_path='track01.flac'",
        }
        rules = Rules(self.app)
        rule = rules.put("sniff", operation["id"], None, selection)
        queued = rules.apply(rule["id"])["queued"][0]["job_id"]
        with self.store.transaction() as db:
            db.execute(
                "UPDATE processing_jobs SET state='failed' WHERE id=?", (queued,)
            )
        self.assertEqual(rules.apply(rule["id"])["counts"]["failed"], 1)
        self.assertEqual(
            rules.apply(rule["id"], retry_failed=True)["queued"][0]["job_id"], queued
        )
        (self.root / "media/track01.flac").write_bytes(b"changed source revision")
        executed = Processing(self.app).run(job_ids=[queued])
        self.assertFalse(executed["complete"])
        self.assertNotEqual(Processing(self.app).get(queued)["state"], "complete")


class ProgrammableRenderTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        from tests.test_artifacts import ArtifactTest

        ArtifactTest.setUpClass.__func__(cls)

    @classmethod
    def tearDownClass(cls):
        cls.fixture.cleanup()

    def setUp(self):
        from tests.test_artifacts import ArtifactTest

        ArtifactTest.setUp(self)

    def test_render_gap_lineage_generated_query_projection_and_repeat(self):
        from catabolic.graphql_query import execute_graphql
        from catabolic.layouts import Layouts
        from catabolic.sql_query import execute_sql

        processing = Processing(self.app)
        processing.enqueue("probe", file_ids=[self.file_id])
        self.assertTrue(processing.run()["complete"])
        queries = Queries(self.store)
        gap = queries.put(
            "missing-proxy",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT item_id FROM catalog_items i WHERE NOT EXISTS (SELECT 1 FROM catalog_rendition_state r WHERE r.profile=:profile AND r.source_item_id=i.item_id AND r.purpose='transcode' AND r.recorded_current=1)",
                }
            },
        )
        operation = Operations(self.app).put(
            "proxy", {"kind": "render", "operation": "h264-720p"}
        )
        rules = Rules(self.app)
        rule = rules.put(
            "proxy",
            operation["id"],
            "generated",
            {"query_id": gap["id"]},
            required=True,
        )
        self.assertEqual(len(queries.run(gap["id"])["ids"]), 1)
        self.assertEqual(len(rules.apply(rule["id"])["queued"]), 1)
        result = rules.run(rule["id"])
        self.assertTrue(result["complete"], result)
        self.assertEqual(queries.run(gap["id"])["ids"], [])
        outputs = execute_sql(
            self.path,
            "SELECT source_file_id,file_id,recorded_current FROM catalog_rendition_state",
            _store=self.store,
        )
        self.assertEqual(outputs["rows"][0][0], self.file_id)
        self.assertEqual(outputs["rows"][0][2], 1)
        gql = execute_graphql(
            self.path,
            '{ items(missingRendition:{purpose:"transcode",current:true}) { nodes { id } } }',
            _store=self.store,
        )
        self.assertEqual(gql["data"]["items"]["nodes"], [])
        generated = queries.put(
            "generated",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT file_id FROM catalog_rendition_state WHERE profile=:profile AND recorded_current=1 AND purpose='transcode'",
                }
            },
        )
        output = self.root / "projection"
        output.mkdir()
        self.app.bind("output", "mobile", str(output))
        Layouts(self.app).put("flat", copy.deepcopy(PRESETS["flat"]))
        projection = Projections(self.app)
        projection.put(
            "mobile", generated["id"], "flat", renditions={"purpose": "transcode"}
        )
        self.assertTrue(projection.run("mobile")["safe"])
        published = projection.run("mobile", apply=True)
        self.assertTrue(published["complete"], published)
        links = [p for p in output.rglob("*") if p.is_symlink()]
        self.assertEqual(len(links), 1)
        self.assertEqual(links[0].resolve().parent, self.generated)
        self.assertEqual(
            projection.run("mobile", apply=True)["layout"]["change_count"], 0
        )
        self.assertEqual(rules.apply(rule["id"])["queued"], [])
        self.assertEqual(self.source_file.read_bytes(), self.payload)

    def test_audio_rendition_is_a_normal_explicit_rule_input(self):
        processing = Processing(self.app)
        processing.enqueue("probe", file_ids=[self.file_id])
        self.assertTrue(processing.run()["complete"])
        flac = self.a.recipe("lossless", "audio-flac")
        self.a.enqueue(self.file_id, flac["id"], "generated", self.item_id)
        output = self.a.run()["completed"][0]
        aac = self.a.recipe("mobile", "audio-aac")
        query = Queries(self.store).put(
            "lossless",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT file_id FROM catalog_rendition_state WHERE operation_id=:operation AND recorded_current=1",
                    "params": {"operation": flac["id"]},
                }
            },
        )
        rules = Rules(self.app)
        rule = rules.put(
            "mobile",
            aac["id"],
            "generated",
            {"query_id": query["id"]},
            allow_derived=True,
        )
        plan = rules.preview(rule["id"])
        self.assertEqual(plan["matched_inputs"], 1)
        self.assertEqual(plan["matches"][0]["file_id"], output["file_id"])
        rules.apply(rule["id"])
        result = rules.run(rule["id"])
        self.assertTrue(result["complete"], result)
        self.assertEqual(rules.apply(rule["id"])["queued"], [])

    def test_explicit_catalog_chaining_and_stale_ancestor(self):
        processing = Processing(self.app)
        processing.enqueue("probe", file_ids=[self.file_id])
        processing.run()
        proxy = self.a.recipe("proxy", "h264-720p")
        self.a.enqueue(self.file_id, proxy["id"], "generated", self.item_id)
        result = self.a.run()
        proxy_file = result["completed"][0]["file_id"]
        queries = Queries(self.store)
        query = queries.put(
            "proxies",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT file_id FROM catalog_rendition_state WHERE purpose='transcode' AND recorded_current=1",
                }
            },
        )
        thumbnail = self.a.recipe("thumb", "thumbnail")
        rules = Rules(self.app)
        default = rules.put(
            "no-implicit-chain", thumbnail["id"], "generated", {"query_id": query["id"]}
        )
        self.assertEqual(rules.preview(default["id"])["matched_inputs"], 0)
        chain = rules.put(
            "chain",
            thumbnail["id"],
            "generated",
            {"query_id": query["id"]},
            allow_derived=True,
        )
        self.assertEqual(len(rules.apply(chain["id"])["queued"]), 1)
        completed = rules.run(chain["id"])
        self.assertTrue(completed["complete"], completed)
        from catabolic.outputs import Outputs
        from catabolic.rendition_publication import ready

        child = Outputs(self.app).get(
            completed["execution"]["completed"][0]["output_id"]
        )
        self.assertEqual(child["source_file_id"], proxy_file)
        ready(self.app, child)
        self.source_file.write_bytes(b"a new source revision")
        with self.assertRaises(CatabolicError):
            ready(self.app, child)


class ProgrammableExternalTest(unittest.TestCase):
    def setUp(self):
        from tests.test_outputs import OutputTest

        OutputTest.setUp(self)

    def test_external_rule_uses_existing_fenced_receipt_jobs(self):
        from catabolic.processors import Processors
        from catabolic.receipts import import_receipt
        from catabolic.sql_query import execute_sql
        from tests.test_processor_workers import ProcessorTest

        self.processors = Processors(self.app)
        self.processors.put("fixture", {"endpoint": "http://127.0.0.1:9"})
        self.definition = self.outputs.define("external", {"purpose": "custom"})
        operation = Operations(self.app).put(
            "external",
            {
                "kind": "external",
                "operation": "fixture",
                "output_definition_id": self.definition["id"],
                "options": {"preset": "fixture"},
            },
        )
        queries = Queries(self.store)
        query = queries.put(
            "missing",
            {
                "selection": {
                    "language": "sql",
                    "query": "SELECT item_id FROM catalog_items i WHERE NOT EXISTS(SELECT 1 FROM catalog_rendition_state r WHERE r.source_item_id=i.item_id AND r.recorded_current=1 AND r.purpose='custom')",
                }
            },
        )
        rules = Rules(self.app)
        rule = rules.put(
            "external", operation["id"], None, {"query_id": query["id"]}, required=True
        )
        self.assertEqual(len(rules.apply(rule["id"])["queued"]), 1)
        self.assertEqual(rules.apply(rule["id"])["queued"], [])
        first = self.processors.claim("fixture", "old-worker")
        with self.store.transaction() as db:
            db.execute("UPDATE processor_jobs SET lease_until=0")
        current = self.processors.claim("fixture", "worker")
        with self.assertRaisesRegex(CatabolicError, "lease"):
            self.processors.complete(
                first["id"], first["lease_token"], ProcessorTest.receipt(self, first)
            )
        receipt = ProcessorTest.receipt(self, current)
        self.processors.complete(current["id"], current["lease_token"], receipt)
        self.assertTrue(import_receipt(self.app, receipt)["reused"])
        from catabolic.item_workflow import ItemWorkflow

        checks = ItemWorkflow(self.app).checks(self.item)["checks"]
        required = [r for r in checks if r["kind"] == "rule"]
        self.assertEqual(len(required), 1)
        self.assertTrue(all(r["passed"] for r in required))
        self.assertEqual(queries.run(query["id"])["ids"], [])
        self.assertEqual(rules.apply(rule["id"])["queued"], [])
        self.assertEqual(len(self.store.rows("SELECT * FROM rule_processor_jobs")), 1)
        self.assertEqual(
            execute_sql(
                self.path,
                "SELECT source_current,output_current,recorded_current FROM catalog_rendition_state",
                _store=self.store,
            )["rows"],
            [[1, 1, 1]],
        )
        self.assertEqual(
            sum(r["count"] for r in rules.statistics(rule["id"])["attempts"]), 2
        )

    def test_rule_cli_external_step_releases_lock_and_stays_in_scope(self):
        import threading
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        from catabolic.processors import Processors
        from catabolic.store import Store
        from tests.test_processor_workers import ProcessorTest

        definition = self.outputs.define("network", {"purpose": "custom"})
        self.definition = definition
        requests = []
        database = self.path
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_PUT(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                with Store(database, writable=True):
                    requests.append(body)
                lease = {
                    "request": body,
                    "id": body["job_id"],
                    "worker": body["instance"],
                    "generation": int(body["attempt"]),
                }
                data = json.dumps(
                    {
                        "state": "complete",
                        "receipt": ProcessorTest.receipt(fixture, lease),
                    }
                ).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        processors = Processors(self.app)
        processors.put(
            "fixture", {"endpoint": f"http://127.0.0.1:{server.server_port}"}
        )
        unrelated = processors.enqueue(
            "fixture",
            self.files["source.mkv"],
            self.item,
            definition["id"],
            {"other": True},
        )
        operation = Operations(self.app).put(
            "network",
            {
                "kind": "external",
                "operation": "fixture",
                "output_definition_id": definition["id"],
            },
        )
        rule = Rules(self.app).put(
            "network",
            operation["id"],
            None,
            {"language": "sql", "query": "SELECT item_id FROM catalog_items"},
        )
        Rules(self.app).apply(rule["id"])
        self.store.close()
        with contextlib.redirect_stdout(io.StringIO()) as output:
            code = main(
                [
                    "--db",
                    str(database),
                    "--machine",
                    "rule",
                    "run",
                    rule["id"],
                    "--worker",
                    "bounded",
                    "--batch",
                    "1",
                ]
            )
        self.assertEqual(code, 0, output.getvalue())
        self.assertEqual(len(requests), 1)
        with Store(database) as store:
            self.assertEqual(
                store.rows(
                    "SELECT state FROM processor_jobs WHERE id=?", (unrelated["id"],)
                ),
                [{"state": "queued"}],
            )
