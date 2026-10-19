# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""New v2 bundle/import composition, independent frozen subprocess oracle.

Only local toy sources, manually assembled fixtures, and read-only previews.
No publisher, import CLI, recovery executor, real catalog or remote service.
"""

import copy
import hashlib
import json
import os
import re
import shutil
import sqlite3
import unittest
from contextlib import closing
from datetime import datetime, timezone
from pathlib import Path

import tests.test_rust_publication_mapping_preview as mapping_preview
from tests.test_rust_legacy_review import REFERENCE
from tests.test_rust_publication_mapping_preview import typed

ORACLE_VERSION = "0.2.0"
CARGO_TOML = Path(__file__).resolve().parents[1] / "Cargo.toml"


def native_version():
    """The workspace crate version the native engine stamps into manifests."""
    match = re.search(
        r'^\[workspace\.package\][^\[]*?^version = "([^"]+)"',
        CARGO_TOML.read_text(),
        re.M | re.S,
    )
    if match is None:
        raise AssertionError("workspace.package version not found in Cargo.toml")
    return match.group(1)


# The frozen plan digests (publication_mappings.plan_digest, digest of an
# export spec, store.encode), restated so each engine's own claimed plan_id and
# signature can be recomputed from the plan it returned.
def encode(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, allow_nan=False)


def digest(value):
    return hashlib.sha256(encode(value).encode()).hexdigest()


def stable_document(document):
    return {k: v for k, v in document.items() if k != "generated_at"}


def plan_digest(plan):
    value = copy.deepcopy(plan)
    if "document" in value:
        value["document"] = stable_document(value["document"])
    data = value.get("data")
    if isinstance(data, dict) and "document" in data:
        data["document"] = stable_document(data["document"])
    elif isinstance(data, list):
        for group in data:
            group["document"] = stable_document(group["document"])
    return digest(value)


def export_signature(plan):
    data = plan["data"]
    return digest({**data, "document": stable_document(data["document"])})


def plan_documents(plan):
    yield plan["document"]
    data = plan.get("data")
    if isinstance(data, dict) and "document" in data:
        yield data["document"]
    elif isinstance(data, list):
        for group in data:
            yield group["document"]


def has_export_signature(plan):
    return isinstance(plan.get("data"), dict) and "signature" in plan


def attest(case, result, version):
    """Check the engine's exact version and that its digests match its plan."""
    plan = result.get("plan")
    if plan is None:
        return
    for document in plan_documents(plan):
        case.assertEqual(
            document["generator"], {"name": "catabolic", "version": version}
        )
    if has_export_signature(plan):
        case.assertEqual(plan["signature"], export_signature(plan))
    case.assertEqual(result["plan_id"], plan_digest(plan))


def rebase(result, version):
    """Restamp the manifest version and recompute only the version-derived digests."""
    result = copy.deepcopy(result)
    plan = result.get("plan")
    if plan is None:
        return result
    for document in plan_documents(plan):
        document["generator"]["version"] = version
    if has_export_signature(plan):
        plan["signature"] = export_signature(plan)
    result["plan_id"] = plan_digest(plan)
    return result


def rebase_envelope(envelope, version):
    if "data" in envelope:
        return {**envelope, "data": rebase(envelope["data"], version)}
    return rebase(envelope, version)


def stable(value):
    if isinstance(value, dict):
        return {
            k: stable(v)
            for k, v in value.items()
            if not (
                k == "generated_at"
                and value.get("format") == "catabolic.catalog-manifest"
            )
        }
    if isinstance(value, list):
        return [stable(v) for v in value]
    return value


class PublicationBundlePreviewTest(unittest.TestCase):
    python = mapping_preview.PublicationMappingPreviewTest.python
    cli = mapping_preview.PublicationMappingPreviewTest.cli
    write = mapping_preview.PublicationMappingPreviewTest.write
    normalize = mapping_preview.PublicationMappingPreviewTest.normalize
    sql = mapping_preview.PublicationMappingPreviewTest.sql
    query = mapping_preview.PublicationMappingPreviewTest.query

    def setUp(self):
        mapping_preview.PublicationMappingPreviewTest.setUp(self)
        # Pin workers launched by frozen Python as well as the outer oracle.
        self.env["PYTHONPATH"] = str(REFERENCE)
        self.destination = self.root / "destination"
        for n in range(3):
            path = self.source / f"book{n}.epub"
            path.write_bytes(f"Disposable toy book {n}\n".encode())
            stat = path.stat()
            self.sql(
                "UPDATE observations SET status='present',size=?,mtime_ns=?,device=?,inode=? WHERE file_id=?",
                (stat.st_size, stat.st_mtime_ns, stat.st_dev, stat.st_ino, f"f{n}"),
            )
            self.sql(
                "INSERT INTO mappings(id,catalog,file_id,item_id,path) VALUES (?,'global',?,?,?)",
                (f"m{n}", f"f{n}", f"i{n}", f"Book {n}/book{n}.epub"),
            )
        self.definition["destination"] = {
            "operation": "export",
            "adapter": "nfo",
            "catalog": "global",
            "path": str(self.destination),
        }
        self.env.pop("IMMICH_API_KEY", None)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        # Linux volume identity still needs findmnt when adapter discovery is
        # restricted. Preserve that prerequisite without exposing real adapters.
        findmnt = shutil.which("findmnt")
        if findmnt:
            (self.bin / "findmnt").symlink_to(findmnt)
        # Discovery only: any execution would be a test failure observable here.
        program = self.bin / "calibredb"
        program.write_text(
            "#!/bin/sh\necho launched > '"
            + str(self.root / "unexpected-launch")
            + "'\nexit 99\n"
        )
        program.chmod(0o755)
        self.env["PATH"] = str(self.bin)

    def snapshot(self):
        state = mapping_preview.PublicationMappingPreviewTest.snapshot(self)
        state["destination"] = [
            (
                str(p.relative_to(self.root)),
                p.lstat().st_mode,
                os.readlink(p)
                if p.is_symlink()
                else hashlib.sha256(p.read_bytes()).hexdigest()
                if p.is_file()
                else "",
            )
            for p in [self.destination, *self.destination.rglob("*")]
            if p.exists() or p.is_symlink()
        ]
        state["launched"] = (self.root / "unexpected-launch").exists()
        return state

    def pair(self, definition=None, stage=None):
        """Run both engines; ``stage(is_oracle)`` may lay out per-engine fixtures."""
        definition = self.definition if definition is None else definition
        path = self.write(definition)
        before = self.snapshot()
        runs = []
        for flag in (True, False):
            if stage:
                stage(flag)
                before = self.snapshot()
            runs.append(
                self.cli(
                    flag,
                    [
                        "projection",
                        "mapping-preview",
                        "--definition",
                        path,
                        "--machine",
                    ],
                )
            )
            if stage:
                self.assertEqual(before, self.snapshot())
        old, new = runs
        self.assertEqual(
            new.returncode,
            old.returncode,
            (old.stdout, old.stderr, new.stdout, new.stderr),
        )
        self.assertEqual(self.normalize(new.stderr), self.normalize(old.stderr))
        a, b = json.loads(old.stdout), json.loads(new.stdout)
        # Each engine stamps its own manifest version (oracle 0.2.0, native the
        # crate version); assert both exactly, then compare every other byte,
        # including the version-derived plan_id and signature, after restamping
        # the native plan to the oracle version and recomputing those digests.
        for value, version in ((a, ORACLE_VERSION), (b, native_version())):
            attest(self, value.get("data", value), version)
        self.assertEqual(
            typed(stable(rebase_envelope(b, ORACLE_VERSION))), typed(stable(a))
        )
        self.assertEqual(before, self.snapshot())
        for value in (a, b):
            result = value.get("data", value)
            if old.returncode == 0:
                generated = result["plan"]["document"]["generated_at"]
                self.assertLess(
                    abs(
                        (
                            datetime.now(timezone.utc)
                            - datetime.fromisoformat(generated)
                        ).total_seconds()
                    ),
                    120,
                )
                self.assertEqual(result["applied"], False)
        return b.get("data", b)

    def event(self, payload, identifier="past", profile="default"):
        self.sql(
            "INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,?,'mapping_publish','info',?)",
            (identifier, profile, json.dumps(payload)),
        )

    def test_three_export_formats_preserve_complete_plan_and_repeat(self):
        for adapter in ("nfo", "opds", "xspf"):
            with self.subTest(adapter=adapter):
                self.definition["destination"].update(
                    adapter=adapter, base_url="https://example.invalid/library/"
                )
                first = self.pair()
                self.assertEqual(first["safe"], True)
                self.assertEqual(len(first["plan"]["sources"]), 3)
                self.assertEqual(first["plan"]["unchanged"], False)
                self.assertEqual(self.pair()["plan_id"], first["plan_id"])

    def test_nfo_ignores_unused_base_url_but_definition_stays_in_identity(self):
        first = self.pair()
        self.definition["destination"]["base_url"] = 37
        second = self.pair()
        self.assertNotEqual(first["plan_id"], second["plan_id"])
        self.assertEqual(first["plan"]["signature"], second["plan"]["signature"])

    def test_calibre_aliases_preserve_groups_and_destination_identity(self):
        self.destination.mkdir()
        for adapter in ("calibre", "calibre-web"):
            with self.subTest(adapter=adapter):
                self.definition["destination"].update(
                    operation="import", adapter=adapter
                )
                result = self.pair()
                self.assertEqual(len(result["plan"]["data"]), 3)
                self.assertEqual(result["plan"]["scope"][1], "calibre")
                self.assertEqual(
                    result["plan"]["destination_identity"]["inode"],
                    self.destination.stat().st_ino,
                )
                self.assertEqual(
                    result["plan"]["executable"], str(self.bin / "calibredb")
                )
                self.assertEqual(self.pair()["plan_id"], result["plan_id"])

    def test_import_submitted_history_dedup_and_revision_guard(self):
        self.definition["destination"].update(operation="import", adapter="calibre")
        first = self.pair()["plan"]
        group = first["data"][0]
        event = {
            "version": 2,
            "state": "submitted",
            "scope": first["scope"],
            "key": group["key"],
            "signature": group["signature"],
            "destination_identity": None,
        }
        self.event(event)
        self.assertEqual(
            [g["unchanged"] for g in self.pair()["plan"]["data"]], [True, False, False]
        )
        self.sql(
            "UPDATE items SET metadata=? WHERE id='i0'",
            (json.dumps({"title": "Changed title", "author": "Author"}),),
        )
        self.assertIn("import_revision_changed_updates_unsupported", str(self.pair()))

    def test_import_destination_identity_and_untransmitted_metadata(self):
        self.definition["destination"].update(operation="import", adapter="calibre")
        first = self.pair()["plan"]
        self.event(
            {
                "version": 2,
                "state": "submitted",
                "scope": first["scope"],
                "key": "i0",
                "signature": first["data"][0]["signature"],
                "destination_identity": None,
            }
        )
        self.sql(
            "UPDATE items SET metadata=? WHERE id='i0'",
            (
                json.dumps(
                    {"title": "Title 0 Café", "author": "Author", "note": "local-only"}
                ),
            ),
        )
        self.assertTrue(self.pair()["plan"]["data"][0]["unchanged"])
        self.destination.mkdir()
        self.assertIn("import_destination_identity_changed", str(self.pair()))

    def test_selection_and_import_unsupported_association(self):
        self.definition["query"] = self.query(
            "SELECT item_id FROM catalog_items WHERE item_id='i1'"
        )
        result = self.pair()
        self.assertEqual(list(result["plan"]["sources"]), ["f1"])
        self.definition["destination"].update(operation="import", adapter="calibre")
        self.sql("UPDATE item_files SET role='cover' WHERE id='a1'")
        self.assertIn("unsupported_import_selection", str(self.pair()))

    def test_export_existing_foreign_destination_and_preflight_collision(self):
        self.destination.mkdir()
        self.assertIn("export_destination_exists", str(self.pair()))
        self.destination.rmdir()
        self.sql("UPDATE mappings SET path='catalog-manifest.json' WHERE id='m0'")
        self.assertIn("export path collision", str(self.pair()))

    def test_export_verified_bundle_history_and_changed_artifact(self):
        native_plan = self.pair()["plan"]
        # The signature and the bundle's manifest bytes embed the engine's own
        # manifest version, so each engine gets history and an on-disk bundle
        # that match its own plan (the oracle one is the same plan restamped).
        oracle_plan = rebase({"plan": native_plan, "plan_id": ""}, ORACLE_VERSION)[
            "plan"
        ]
        data = native_plan["data"]
        self.destination.mkdir()
        for entry in data["document"]["content"]["entries"]:
            target = self.destination / entry["path"]
            target.parent.mkdir(parents=True, exist_ok=True)
            source = data["sources"][entry["file_id"]]
            target.symlink_to(
                os.path.relpath(self.source / source["path"], target.parent)
            )

        def lay_out(is_oracle):
            plan = oracle_plan if is_oracle else native_plan
            for artifact in [
                *plan["data"]["export"]["files"],
                {
                    "path": "catalog-manifest.json",
                    "content": json.dumps(
                        plan["data"]["document"], sort_keys=True, ensure_ascii=False
                    )
                    + "\n",
                },
            ]:
                target = self.destination / artifact["path"]
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(artifact["content"])

        lay_out(False)
        stat = self.destination.stat()
        for is_oracle, plan in ((True, oracle_plan), (False, native_plan)):
            with closing(sqlite3.connect(self.db[is_oracle])) as db:
                db.execute(
                    "INSERT INTO consumer_events(id,profile,event,severity,subject) VALUES (?,?,'mapping_publish','info',?)",
                    (
                        "past",
                        "default",
                        json.dumps(
                            {
                                "version": 2,
                                "state": "verified",
                                "scope": plan["scope"],
                                "signature": plan["signature"],
                                "owner": self.definition["id"],
                                "data": plan["data"],
                                "output_identity": {
                                    "device": stat.st_dev,
                                    "inode": stat.st_ino,
                                },
                            }
                        ),
                    ),
                )
                db.commit()
        self.assertTrue(self.pair(stage=lay_out)["plan"]["unchanged"])
        (self.destination / "catalog-manifest.json").write_text("changed toy fixture")
        self.assertIn("owned_export_changed", str(self.pair()))

    def test_pending_history_and_empty_selection(self):
        self.definition["query"] = self.query(
            "SELECT item_id FROM catalog_items WHERE 0"
        )
        self.assertEqual(self.pair()["plan"]["sources"], {})
        self.event(
            {
                "version": 2,
                "state": "uncertain",
                "scope": ["export", str(self.destination)],
            }
        )
        self.assertIn("unresolved_write_recover_past", str(self.pair()))

    def test_apply_and_recovery_still_refuse_before_bundle_effects(self):
        # Folder publication is open (track C4); export and import apply stay
        # gated. With the reviewed plan, a safe plan reaches the gate after
        # frozen admission and an unsafe one returns unapplied; a stale plan
        # is refused as in frozen. Nothing is written either way.
        before = self.snapshot()
        for operation, adapter in [
            ("export", "nfo"),
            ("import", "calibre"),
            ("import", "immich"),
        ]:
            definition = copy.deepcopy(self.definition)
            definition["destination"].update(operation=operation, adapter=adapter)
            path = self.write(definition)
            preview = self.cli(
                False,
                ["projection", "mapping-preview", "--definition", path, "--machine"],
            )
            reviewed = json.loads(preview.stdout).get("data", {})
            # A preview that fails (this immich fixture has no valid base URL)
            # fails apply identically, before any plan comparison.
            failure = reviewed["error"]["message"] if preview.returncode else None
            for expected in ("stale", reviewed.get("plan_id")):
                if expected is None:
                    continue
                with self.subTest(operation=operation, adapter=adapter, plan=expected):
                    result = self.cli(
                        False,
                        [
                            "projection",
                            "mapping-apply",
                            "--definition",
                            path,
                            "--expected-plan",
                            expected,
                            "--machine",
                        ],
                    )
                    if failure is not None:
                        self.assertEqual(result.returncode, 2, result.stdout)
                        self.assertIn(failure, result.stdout)
                    elif expected == "stale":
                        self.assertEqual(result.returncode, 2, result.stdout)
                        self.assertIn("stale_plan_preview_again", result.stdout)
                    elif reviewed["safe"]:
                        self.assertEqual(result.returncode, 2, result.stdout)
                        self.assertIn(
                            "publication mapping apply/recover is blocked",
                            result.stdout,
                        )
                    else:
                        self.assertFalse(
                            json.loads(result.stdout)["data"]["applied"], result.stdout
                        )
        self.assertEqual(before, self.snapshot())

    def serve_identity(self):
        import http.server
        import threading

        owner = self
        self.requests = []
        self.response = (
            200,
            {},
            json.dumps(
                {"id": "toy-account-A", "email": "never-retained@example.invalid"}
            ).encode(),
        )

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                owner.requests.append((self.path, self.headers.get("x-api-key")))
                if getattr(owner, "identity_started", None) is not None:
                    owner.identity_started.set()
                    owner.identity_release.wait(8)
                code, headers, body = owner.response
                self.send_response(code)
                for key, value in headers.items():
                    self.send_header(key, value)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def do_POST(self):
                owner.requests.append(("UNEXPECTED POST", None))
                self.send_error(405)

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        self.env["IMMICH_API_KEY"] = "toy-not-a-secret"
        self.definition["destination"].update(
            operation="import",
            adapter="immich",
            path=f"http://127.0.0.1:{server.server_port}/api",
        )
        self.sql("UPDATE items SET kind='photo'")
        return server

    def test_immich_identity_scope_dedup_and_account_change(self):
        self.serve_identity()
        first = self.pair()["plan"]
        self.assertEqual(first["scope"][-1], "toy-account-A")
        self.assertEqual(first["destination_identity"], {"user_id": "toy-account-A"})
        self.assertEqual(self.requests, [("/api/users/me", "toy-not-a-secret")] * 2)
        self.assertNotIn("email", json.dumps(first))
        self.assertNotIn("toy-not-a-secret", json.dumps(first))
        group = first["data"][0]
        self.event(
            {
                "version": 2,
                "state": "submitted",
                "scope": first["scope"],
                "key": group["key"],
                "signature": group["signature"],
                "destination_identity": first["destination_identity"],
            }
        )
        self.assertTrue(self.pair()["plan"]["data"][0]["unchanged"])
        self.sql("UPDATE items SET metadata='{}'")
        self.assertTrue(self.pair()["plan"]["data"][0]["unchanged"])
        self.response = (200, {}, b'{"id":"toy-account-B"}')
        changed = self.pair()["plan"]
        self.assertEqual(changed["scope"][-1], "toy-account-B")
        self.assertFalse(any(g["unchanged"] for g in changed["data"]))

    def test_immich_legacy_history_refuses_before_identity_request(self):
        self.serve_identity()
        scope = ["import", "immich", self.definition["destination"]["path"]]
        self.event({"version": 2, "state": "submitted", "scope": scope})
        self.assertIn("legacy_import_account_identity_unknown", str(self.pair()))
        self.assertEqual(self.requests, [])

    def test_immich_invalid_identity_status_and_redirect_errors(self):
        self.serve_identity()
        for body in (
            b"{}",
            b'{"id":""}',
            b'{"id":true}',
            b"[]",
            b"not JSON",
            json.dumps({"id": "x" * 129}).encode(),
        ):
            with self.subTest(body=body):
                self.response = (200, {}, body)
                self.assertIn("invalid_import_account_identity", str(self.pair()))
        for code in (400, 401, 403, 404, 405, 412, 422, 429, 500, 501):
            with self.subTest(status=code):
                self.response = (code, {}, b"no private details")
                self.pair()
        self.response = (302, {"Location": "http://127.0.0.1:1/never"}, b"")
        self.assertIn("temporarily_failed", str(self.pair()))
        self.response = (200, {}, b"x" * (1024 * 1024 + 1))
        self.assertIn("temporarily_failed", str(self.pair()))

    def test_import_multiformat_groups_and_duplicate_format_refusal(self):
        self.definition["destination"].update(operation="import", adapter="calibre")
        self.sql("UPDATE item_files SET item_id='i0' WHERE id='a1'")
        self.sql("UPDATE mappings SET item_id='i0' WHERE id='m1'")
        self.assertIn("multiple copies of one book format", str(self.pair()))
        (self.source / "book1.epub").rename(self.source / "book1.pdf")
        self.sql("UPDATE files SET path='book1.pdf' WHERE id='f1'")
        self.sql("UPDATE mappings SET path='Book 1/book1.pdf' WHERE id='m1'")
        plan = self.pair()["plan"]
        self.assertEqual([g["key"] for g in plan["data"]], ["i0", "i2"])
        self.assertEqual(list(plan["data"][0]["sources"]), ["f0", "f1"])
        self.assertEqual(len(plan["data"][0]["document"]["content"]["items"]), 2)

    def test_document_limit_and_source_revision_dedup_refusal(self):
        self.definition["destination"].update(operation="import", adapter="calibre")
        plan = self.pair()["plan"]
        self.event(
            {
                "version": 2,
                "state": "submitted",
                "scope": plan["scope"],
                "key": "i0",
                "signature": plan["data"][0]["signature"],
                "destination_identity": None,
            }
        )
        path = self.source / "book0.epub"
        path.write_bytes(b"A revised disposable toy book")
        stat = path.stat()
        self.sql(
            "UPDATE observations SET size=?,mtime_ns=?,device=?,inode=? WHERE file_id='f0'",
            (stat.st_size, stat.st_mtime_ns, stat.st_dev, stat.st_ino),
        )
        self.assertIn("import_revision_changed_updates_unsupported", str(self.pair()))
        self.sql(
            "UPDATE items SET metadata=? WHERE id='i0'",
            (json.dumps({"title": "x" * (4 * 1024 * 1024)}),),
        )
        self.assertIn("publication_plan_exceeds_limit", str(self.pair()))

    def test_immich_cancellation_stops_read_only_helper(self):
        import signal
        import subprocess
        import threading
        import time

        from tests.test_rust_legacy_review import BINARY

        self.serve_identity()
        started = threading.Event()
        release = threading.Event()
        self.addCleanup(release.set)
        self.identity_started = started
        self.identity_release = release
        before = self.snapshot()
        process = subprocess.Popen(
            [
                str(BINARY),
                "--machine",
                "--db",
                str(self.db[False]),
                "projection",
                "mapping-preview",
                "--definition",
                self.write(self.definition),
            ],
            env=self.env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        def cleanup():
            if process.poll() is None:
                process.kill()
            process.communicate()

        self.addCleanup(cleanup)
        self.assertTrue(started.wait(10), "identity request never arrived")
        then = time.monotonic()
        process.send_signal(signal.SIGINT)
        stdout, stderr = process.communicate(timeout=5)
        release.set()
        self.assertEqual(process.returncode, 130, (stdout, stderr))
        self.assertLess(time.monotonic() - then, 5)
        self.assertEqual(before, self.snapshot())
