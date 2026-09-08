# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Document adapter limits, provenance and durable text indexing."""

import shutil
import struct
import sys
import threading
import unittest
import zlib
from unittest.mock import patch

from catabolic import document_text
from catabolic.domain import CatabolicError
from catabolic.process_runner import CommandFailure, command_output
from catabolic.processing import current_fact, operation_config
from tests import test_enrichment


def pdf_fixture(pages):
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        (
            "<< /Type /Pages /Kids ["
            + " ".join(f"{4 + 2 * i} 0 R" for i in range(len(pages)))
            + f"] /Count {len(pages)} >>"
        ).encode(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ]
    for i, text in enumerate(pages):
        content = f"BT /F1 24 Tf 40 200 Td ({text}) Tj ET".encode()
        objects.extend(
            [
                f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Resources << /Font << /F1 3 0 R >> >> /Contents {5 + 2 * i} 0 R >>".encode(),
                f"<< /Length {len(content)} >>\nstream\n".encode()
                + content
                + b"\nendstream",
            ]
        )
    data, offsets = b"%PDF-1.4\n", [0]
    for i, obj in enumerate(objects, 1):
        offsets.append(len(data))
        data += f"{i} 0 obj\n".encode() + obj + b"\nendobj\n"
    xref = len(data)
    data += f"xref\n0 {len(offsets)}\n0000000000 65535 f \n".encode()
    data += b"".join(f"{offset:010} 00000 n \n".encode() for offset in offsets[1:])
    return (
        data
        + f"trailer\n<< /Size {len(offsets)} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    )


class DocumentTextTest(unittest.TestCase):
    setUp = test_enrichment.EnrichmentTest.setUp

    @unittest.skipUnless(shutil.which("tesseract"), "optional tesseract required")
    def test_real_ocr_image(self):
        glyphs = {
            "C": ("01110", "10001", "10000", "10000", "10000", "10001", "01110"),
            "A": ("01110", "10001", "10001", "11111", "10001", "10001", "10001"),
            "T": ("11111", "00100", "00100", "00100", "00100", "00100", "00100"),
            "L": ("10000", "10000", "10000", "10000", "10000", "10000", "11111"),
            "O": ("01110", "10001", "10001", "10001", "10001", "10001", "01110"),
            "G": ("01110", "10001", "10000", "10111", "10001", "10001", "01110"),
        }
        width, height, scale = 520, 130, 10
        pixels = bytearray([255]) * (width * height)
        for ordinal, letter in enumerate("CATALOG"):
            for row, values in enumerate(glyphs[letter]):
                for column, bit in enumerate(values):
                    if bit == "1":
                        for y in range(30 + row * scale, 30 + (row + 1) * scale):
                            start = y * width + 30 + (ordinal * 7 + column) * scale
                            pixels[start : start + scale] = bytes(scale)

        def chunk(kind, data):
            return (
                struct.pack(">I", len(data))
                + kind
                + data
                + struct.pack(">I", zlib.crc32(kind + data))
            )

        raw = b"".join(
            b"\0" + pixels[y * width : (y + 1) * width] for y in range(height)
        )
        payload = (
            b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw))
            + chunk(b"IEND", b"")
        )
        self.file.write_bytes(payload)
        self.app.scan()
        self.p.enqueue("text", file_ids=[self.file_id], options={"backend": "ocr"})
        self.assertTrue(self.p.run()["complete"])
        self.assertEqual(len(self.p.search("catalog")["hits"]), 1)
        self.assertEqual(self.file.read_bytes(), payload)

    def test_options_are_explicit_bounded_and_discoverable(self):
        options = {"backend": "pdf"}
        operation_config("text", options)
        self.assertEqual(options, {"backend": "pdf"})
        for operation, options in (
            ("probe", {"backend": "pdf"}),
            ("text", {"backend": []}),
            ("text", {"backend": "pdf", "max_pages": True}),
            ("text", {"backend": "pdf", "max_pages": 1001}),
            ("text", {"backend": "utf8", "language": "eng"}),
            ("text", {"backend": "ocr", "language": "../../config"}),
            ("text", {"backend": "ocr", "language": "eng -y"}),
        ):
            with self.subTest(options=options), self.assertRaises(CatabolicError):
                operation_config(operation, options)
        self.assertEqual(operation_config("text")["extractor"], {"adapter": "text-v1"})
        self.assertEqual(operation_config("text", {"backend": "pdf"})["max_pages"], 100)

    def test_document_results_index_pages_and_invalidate_on_change(self):
        self.file.write_bytes(pdf_fixture(["Catalog alpha", "Catalog beta"]))
        original = self.file.read_bytes()
        self.app.scan()
        identity = {"adapter": "fixture-pdf", "tool": "fixture", "available": True}
        with (
            patch.object(document_text, "tool_identity", return_value=identity),
            patch.object(
                document_text,
                "command_output",
                return_value=b"Catalog alpha\n\fCatalog beta\n\f",
            ) as call,
        ):
            self.p.enqueue("text", file_ids=[self.file_id], options={"backend": "pdf"})
            self.assertTrue(self.p.run()["complete"])
        self.assertEqual(call.call_args.kwargs["maximum"], 2 * 1024 * 1024)
        hits = self.p.search("beta")["hits"]
        self.assertEqual(len(hits), 1)
        self.assertEqual(hits[0]["locator"], {"page": 2})
        self.assertEqual(self.file.read_bytes(), original)
        self.assertTrue(
            current_fact(self.store, self.app.profile, self.file_id, "text")["current"]
        )
        self.file.write_bytes(original + b"\n")
        self.app.scan()
        self.assertFalse(
            current_fact(self.store, self.app.profile, self.file_id, "text")["current"]
        )
        self.assertTrue(
            all(not hit["current"] for hit in self.p.search("beta")["hits"])
        )

    def test_failed_partial_or_changed_extraction_never_indexes(self):
        self.file.write_bytes(pdf_fixture(["alpha", "beta"]))
        self.app.scan()
        identity = {"adapter": "fixture-pdf", "tool": "fixture", "available": True}
        for payload in (b"alpha\fextra\f", b"", b"\xff"):
            with (
                self.subTest(payload=payload),
                patch.object(document_text, "tool_identity", return_value=identity),
                patch.object(document_text, "command_output", return_value=payload),
            ):
                self.p.enqueue(
                    "text",
                    file_ids=[self.file_id],
                    options={"backend": "pdf", "max_pages": 1},
                    refresh=True,
                )
                self.assertFalse(self.p.run()["complete"])
                self.assertEqual(self.store.rows("SELECT * FROM text_segments"), [])
        with patch.object(document_text, "tool_identity", return_value=identity):
            self.p.enqueue(
                "text",
                file_ids=[self.file_id],
                options={"backend": "pdf"},
                refresh=True,
            )
        with (
            patch.object(
                document_text,
                "tool_identity",
                return_value={**identity, "version": "changed"},
            ),
            patch.object(document_text, "command_output") as call,
        ):
            self.assertFalse(self.p.run()["complete"])
            call.assert_not_called()

    def test_missing_extractor_reports_unsupported(self):
        with patch.object(document_text.shutil, "which", return_value=None):
            self.p.enqueue("text", file_ids=[self.file_id], options={"backend": "ocr"})
            self.assertFalse(self.p.run()["complete"])
        self.assertIsNone(
            current_fact(self.store, self.app.profile, self.file_id, "text")
        )
        self.assertEqual(
            self.store.rows("SELECT state FROM processing_jobs")[0]["state"],
            "unsupported",
        )

    def test_input_limits_and_signatures_precede_launch(self):
        identity = {"adapter": "fixture", "tool": "fixture", "available": True}
        with (
            patch.object(document_text, "tool_identity", return_value=identity),
            patch.object(document_text, "command_output") as call,
        ):
            for backend, maximum in (("pdf", 1), ("pdf", 8192), ("ocr", 8192)):
                config = operation_config(
                    "text", {"backend": backend, "max_bytes": maximum}
                )
                with (
                    self.subTest(backend=backend, maximum=maximum),
                    self.file.open("rb") as source,
                    self.assertRaises(CommandFailure),
                ):
                    document_text.extract(source.fileno(), config, threading.Event())
            call.assert_not_called()

    def test_ocr_retains_provenance_and_image_location(self):
        self.file.write_bytes(b"\x89PNG\r\n\x1a\nfixture")
        self.app.scan()
        identity = {"adapter": "fixture-ocr", "tool": "fixture", "available": True}
        with (
            patch.object(document_text, "tool_identity", return_value=identity),
            patch.object(
                document_text, "command_output", return_value=b"Catalog alpha\n"
            ) as call,
        ):
            self.p.enqueue(
                "text",
                file_ids=[self.file_id],
                options={"backend": "ocr", "language": "eng+fra"},
            )
            self.assertTrue(self.p.run()["complete"])
        self.assertIn("eng+fra", call.call_args.args[0])
        self.assertEqual(call.call_args.kwargs["env"]["OMP_THREAD_LIMIT"], "1")
        fact = current_fact(self.store, self.app.profile, self.file_id, "text")
        self.assertEqual(fact["data"]["extractor"], identity)
        self.assertEqual(fact["data"]["segments"][0]["locator"], {"page": 1, "line": 1})

    def test_subprocess_stderr_capture_remains_bounded(self):
        argv = [
            sys.executable,
            "-c",
            "import sys; print('out'); print('err',file=sys.stderr)",
        ]
        self.assertEqual(command_output(argv, include_stderr=True), b"out\nerr\n")
        self.assertEqual(command_output(argv), b"out\n")
        with self.assertRaises(CommandFailure):
            command_output(argv, include_stderr=True, maximum=4)

    @unittest.skipUnless(shutil.which("pdftotext"), "optional pdftotext required")
    def test_real_pdf_text_and_page_overflow(self):
        self.file.write_bytes(pdf_fixture(["Catalog alpha", "Catalog beta"]))
        self.app.scan()
        self.p.enqueue("text", file_ids=[self.file_id], options={"backend": "pdf"})
        self.assertTrue(self.p.run()["complete"])
        self.assertEqual(self.p.search("beta")["hits"][0]["locator"], {"page": 2})
        self.p.enqueue(
            "text",
            file_ids=[self.file_id],
            options={"backend": "pdf", "max_pages": 1},
            refresh=True,
        )
        self.assertFalse(self.p.run()["complete"])
        self.assertEqual(
            len(self.p.search("beta")["hits"]), 1
        )  # Prior complete evidence is preserved.
