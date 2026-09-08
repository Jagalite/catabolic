# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Opt-in document adapters over validated source descriptors and bounded stdout."""

import os
import shutil
from pathlib import Path

from .process_runner import CommandFailure, command_output


def tool_identity(backend):
    from .processing import tool_version

    executable = "pdftotext" if backend == "pdf" else "tesseract"
    identity = {
        "adapter": "text-" + backend + "-v1",
        "tool": executable,
        "available": False,
    }
    found = shutil.which(executable)
    if not found:
        return identity
    path = str(Path(found).resolve())
    st = os.stat(path)
    identity.update(
        tool=path,
        available=True,
        size=st.st_size,
        mtime_ns=st.st_mtime_ns,
        version=tool_version(
            path, st.st_size, st.st_mtime_ns, "-v" if backend == "pdf" else "--version"
        ),
    )
    return identity


def extract(fd, options, cancel):
    backend = options["backend"]
    identity = options["extractor"]
    if identity != tool_identity(backend):
        raise CommandFailure(
            "changed", "document extractor changed since enqueue; enqueue a new job"
        )
    if not identity.get("available"):
        raise CommandFailure(
            "unsupported", f"optional tool {identity['tool']} is unavailable"
        )
    if os.fstat(fd).st_size > options["max_bytes"]:
        raise CommandFailure("partial", "document input exceeds the byte limit")
    signature = os.read(fd, 16)
    os.lseek(fd, 0, os.SEEK_SET)
    path = f"/dev/fd/{fd}"
    if backend == "pdf":
        if not signature.startswith(b"%PDF-"):
            raise CommandFailure("unsupported", "pdf backend requires a PDF signature")
        argv = [
            identity["tool"],
            "-enc",
            "UTF-8",
            "-f",
            "1",
            "-l",
            str(options["max_pages"] + 1),
            path,
            "-",
        ]
    else:
        if not (
            signature.startswith(b"\x89PNG\r\n\x1a\n")
            or signature.startswith(b"\xff\xd8\xff")
        ):
            raise CommandFailure(
                "unsupported", "ocr backend supports single PNG or JPEG images"
            )
        argv = [
            identity["tool"],
            path,
            "stdout",
            "-l",
            options["language"],
            "--psm",
            "3",
        ]
    env = dict(os.environ)
    env["OMP_THREAD_LIMIT"] = "1"
    raw = command_output(
        argv,
        pass_fds=(fd,),
        timeout=options["timeout"],
        maximum=options["max_bytes"],
        cancel=cancel,
        env=env,
    )
    text = raw.decode("utf-8-sig")
    if backend == "pdf":
        pages = text.split("\f")
        if pages[-1] == "":
            pages.pop()
        if len(pages) > options["max_pages"]:
            raise CommandFailure(
                "partial", "PDF exceeds the page limit; no partial text was indexed"
            )
        segments = [
            {"locator": {"page": i + 1}, "text": page.strip()}
            for i, page in enumerate(pages)
            if page.strip()
        ]
    else:
        segments = [
            {"locator": {"page": 1, "line": i + 1}, "text": line}
            for i, line in enumerate(text.splitlines())
            if line.strip()
        ]
    if len(segments) > 10000:
        raise CommandFailure("partial", "document exceeds 10000 text segments")
    if not segments:
        raise CommandFailure(
            "unsupported",
            "no text found; this does not establish that the document contains no text",
        )
    return {
        "segments": segments,
        "backend": backend,
        "extractor": identity,
        "coverage": "embedded PDF text; scanned pages need separate OCR"
        if backend == "pdf"
        else "OCR of one raster image; recognition accuracy is not verified",
    }
